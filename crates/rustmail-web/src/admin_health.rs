//! The system health page (Phase 7).
//!
//! The web server and the mail daemons may live on different machines (in
//! development, the mail stack is in a container), so most checks are *probes of
//! the real thing*: connect, and see whether the service answers. Where a probe is
//! not possible from this host (no `systemctl`, no `postqueue`, no configured
//! address) the check says "not checked" instead of guessing "down".
//!
//! Every external command is a fixed program with fixed arguments (paths come from
//! the server's own configuration, never from a request) and has a short timeout.

use std::path::Path;
use std::time::Duration;

use rustmail_core::db;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpStream, UnixStream};
use tokio::process::Command;
use tokio::time::timeout;

use crate::app::AppState;

const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// systemd units worth showing when `systemctl` is available.
const UNITS: [&str; 8] = [
    "postgresql",
    "redis-server",
    "postfix",
    "dovecot",
    "amavis",
    "clamav-daemon",
    "clamav-freshclam",
    "rustmail-web",
];

/// How a check turned out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Working.
    Ok,
    /// Working but needs attention.
    Warn,
    /// Not working.
    Down,
    /// Could not be checked from this machine.
    Unchecked,
}

impl Level {
    /// The Bootstrap colour name.
    #[must_use]
    pub fn class(self) -> &'static str {
        match self {
            Self::Ok => "success",
            Self::Warn => "warning",
            Self::Down => "danger",
            Self::Unchecked => "secondary",
        }
    }

    /// The translation key of its label.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Ok => "admin-health-ok",
            Self::Warn => "admin-health-warn",
            Self::Down => "admin-health-down",
            Self::Unchecked => "admin-health-unchecked",
        }
    }
}

/// One line of the health page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// What was checked (a translation key).
    pub label_key: &'static str,
    /// An extra name, for example the systemd unit.
    pub subject: String,
    /// The outcome.
    pub level: Level,
    /// A short factual detail.
    pub detail: String,
}

fn check(
    label_key: &'static str,
    subject: impl Into<String>,
    level: Level,
    detail: impl Into<String>,
) -> Check {
    Check {
        label_key,
        subject: subject.into(),
        level,
        detail: detail.into(),
    }
}

/// Reads the first line a server sends on connect.
async fn greeting(stream: TcpStream) -> std::io::Result<String> {
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).await?;
    Ok(line.trim().chars().take(80).collect())
}

async fn probe_tcp(label_key: &'static str, host: &str, port: u16, expect: &str) -> Check {
    let subject = format!("{host}:{port}");
    let result = timeout(PROBE_TIMEOUT, async {
        let stream = TcpStream::connect((host, port)).await?;
        greeting(stream).await
    })
    .await;
    match result {
        Ok(Ok(line)) if line.starts_with(expect) => check(label_key, subject, Level::Ok, line),
        Ok(Ok(line)) => check(
            label_key,
            subject,
            Level::Warn,
            format!("unexpected greeting: {line}"),
        ),
        Ok(Err(error)) => check(label_key, subject, Level::Down, error.to_string()),
        Err(_) => check(
            label_key,
            subject,
            Level::Down,
            "no answer within 3 seconds",
        ),
    }
}

async fn database(state: &AppState) -> Check {
    match db::ping(&state.pool).await {
        Ok(elapsed) => check(
            "admin-health-database",
            "PostgreSQL",
            Level::Ok,
            format!("{} ms", elapsed.as_millis()),
        ),
        Err(error) => check(
            "admin-health-database",
            "PostgreSQL",
            Level::Down,
            error.to_string(),
        ),
    }
}

async fn redis() -> Check {
    let Some(url) = std::env::var("REDIS_URL").ok().filter(|u| !u.is_empty()) else {
        return check(
            "admin-health-redis",
            "Redis",
            Level::Unchecked,
            "REDIS_URL is not set",
        );
    };
    let authority = url
        .split("://")
        .nth(1)
        .unwrap_or(&url)
        .rsplit('@')
        .next()
        .unwrap_or("")
        .split('/')
        .next()
        .unwrap_or("");
    let (host, port) = authority
        .rsplit_once(':')
        .map_or((authority, 6379), |(h, p)| (h, p.parse().unwrap_or(6379)));
    let result = timeout(PROBE_TIMEOUT, async {
        let mut stream = TcpStream::connect((host, port)).await?;
        stream.write_all(b"PING\r\n").await?;
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).await?;
        Ok::<_, std::io::Error>(reply)
    })
    .await;
    match result {
        Ok(Ok(reply)) if reply.starts_with("+PONG") => {
            check("admin-health-redis", "Redis", Level::Ok, "PONG")
        }
        // Password-protected Redis answers with an error, which still proves it is up.
        Ok(Ok(reply)) if reply.starts_with("-NOAUTH") => check(
            "admin-health-redis",
            "Redis",
            Level::Ok,
            "up (needs authentication)",
        ),
        Ok(Ok(reply)) => check(
            "admin-health-redis",
            "Redis",
            Level::Warn,
            reply.trim().chars().take(60).collect::<String>(),
        ),
        Ok(Err(error)) => check(
            "admin-health-redis",
            "Redis",
            Level::Down,
            error.to_string(),
        ),
        Err(_) => check(
            "admin-health-redis",
            "Redis",
            Level::Down,
            "no answer within 3 seconds",
        ),
    }
}

async fn clamd() -> Check {
    let Some(path) = std::env::var("CLAMD_SOCKET").ok().filter(|p| !p.is_empty()) else {
        return check(
            "admin-health-clamav",
            "ClamAV",
            Level::Unchecked,
            "CLAMD_SOCKET is not set",
        );
    };
    if !Path::new(&path).exists() {
        return check(
            "admin-health-clamav",
            "ClamAV",
            Level::Unchecked,
            "not on this machine",
        );
    }
    let result = timeout(PROBE_TIMEOUT, async {
        let mut stream = UnixStream::connect(&path).await?;
        stream.write_all(b"PING\n").await?;
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).await?;
        Ok::<_, std::io::Error>(reply)
    })
    .await;
    match result {
        Ok(Ok(reply)) if reply.trim() == "PONG" => {
            check("admin-health-clamav", "ClamAV", Level::Ok, "PONG")
        }
        Ok(Ok(reply)) => check(
            "admin-health-clamav",
            "ClamAV",
            Level::Warn,
            reply.trim().chars().take(60).collect::<String>(),
        ),
        Ok(Err(error)) => check(
            "admin-health-clamav",
            "ClamAV",
            Level::Down,
            error.to_string(),
        ),
        Err(_) => check(
            "admin-health-clamav",
            "ClamAV",
            Level::Down,
            "no answer within 3 seconds",
        ),
    }
}

async fn amavis() -> Check {
    let Some(addr) = std::env::var("HEALTH_AMAVIS_ADDR")
        .ok()
        .filter(|a| !a.is_empty())
    else {
        return check(
            "admin-health-amavis",
            "Amavis",
            Level::Unchecked,
            "HEALTH_AMAVIS_ADDR is not set",
        );
    };
    match addr
        .rsplit_once(':')
        .and_then(|(h, p)| p.parse::<u16>().ok().map(|p| (h.to_owned(), p)))
    {
        Some((host, port)) => probe_tcp("admin-health-amavis", &host, port, "220").await,
        None => check(
            "admin-health-amavis",
            "Amavis",
            Level::Unchecked,
            "HEALTH_AMAVIS_ADDR must look like host:port",
        ),
    }
}

/// Runs a fixed program with fixed arguments and returns its standard output.
async fn run(program: &str, args: &[&str]) -> Option<String> {
    let output = timeout(
        PROBE_TIMEOUT,
        Command::new(program).args(args).kill_on_drop(true).output(),
    )
    .await
    .ok()?
    .ok()?;
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

async fn postfix_queue() -> Check {
    for program in ["postqueue", "/usr/sbin/postqueue"] {
        if let Some(out) = run(program, &["-j"]).await {
            let queued = out
                .lines()
                .filter(|l| l.trim_start().starts_with('{'))
                .count();
            let level = match queued {
                0..=99 => Level::Ok,
                100..=999 => Level::Warn,
                _ => Level::Down,
            };
            return check(
                "admin-health-queue",
                "Postfix",
                level,
                format!("{queued} message(s) queued"),
            );
        }
    }
    check(
        "admin-health-queue",
        "Postfix",
        Level::Unchecked,
        "postqueue is not on this machine",
    )
}

async fn pipeline(state: &AppState, admin: &rustmail_core::admin::Admin) -> Check {
    match state.admin.pipeline_snapshot(admin).await {
        Ok(r) => match r.last_read {
            None => check(
                "admin-health-pipeline",
                "rustmail-cli pipeline",
                Level::Unchecked,
                "no log has been read yet",
            ),
            Some(at) => {
                let age = (chrono::Utc::now() - at).num_seconds();
                let level = if age > 3600 { Level::Warn } else { Level::Ok };
                check(
                    "admin-health-pipeline",
                    "rustmail-cli pipeline",
                    level,
                    format!(
                        "last read {} ago; {} event(s) in the last hour; {} held in quarantine",
                        ago(age),
                        r.events_last_hour,
                        r.held
                    ),
                )
            }
        },
        Err(error) => check(
            "admin-health-pipeline",
            "rustmail-cli pipeline",
            Level::Down,
            error.to_string(),
        ),
    }
}

fn ago(seconds: i64) -> String {
    match seconds {
        s if s < 90 => format!("{s} s"),
        s if s < 5400 => format!("{} min", s / 60),
        s => format!("{} h", s / 3600),
    }
}

async fn disk(label: &'static str, path: &str) -> Check {
    if !Path::new(path).is_dir() {
        return check(
            "admin-health-disk",
            label,
            Level::Unchecked,
            "not on this machine",
        );
    }
    // POSIX output: Filesystem 1024-blocks Used Available Capacity Mounted-on
    let Some(out) = run("df", &["-Pk", path]).await else {
        return check(
            "admin-health-disk",
            label,
            Level::Unchecked,
            "df is not available",
        );
    };
    let line = out.lines().nth(1).unwrap_or("");
    let fields: Vec<&str> = line.split_whitespace().collect();
    let percent = fields
        .get(4)
        .and_then(|p| p.trim_end_matches('%').parse::<u32>().ok());
    let free_kb = fields.get(3).and_then(|p| p.parse::<i64>().ok());
    match (percent, free_kb) {
        (Some(p), Some(free)) => {
            let level = match p {
                0..=84 => Level::Ok,
                85..=94 => Level::Warn,
                _ => Level::Down,
            };
            check(
                "admin-health-disk",
                label,
                level,
                format!(
                    "{p}% used, {} free",
                    crate::admin_common::fmt_bytes(free * 1024)
                ),
            )
        }
        _ => check(
            "admin-health-disk",
            label,
            Level::Unchecked,
            "could not read the figures",
        ),
    }
}

async fn systemd() -> Vec<Check> {
    if !Path::new("/run/systemd/system").exists() {
        return vec![check(
            "admin-health-systemd",
            "systemd",
            Level::Unchecked,
            "this machine does not run systemd",
        )];
    }
    let mut checks = Vec::new();
    for unit in UNITS {
        let Some(out) = run(
            "systemctl",
            &[
                "show",
                "-p",
                "LoadState",
                "-p",
                "ActiveState",
                "--",
                &format!("{unit}.service"),
            ],
        )
        .await
        else {
            return vec![check(
                "admin-health-systemd",
                "systemd",
                Level::Unchecked,
                "systemctl is not available",
            )];
        };
        let value = |key: &str| {
            out.lines()
                .find_map(|l| l.strip_prefix(&format!("{key}=")))
                .unwrap_or("")
                .to_owned()
        };
        if value("LoadState") == "not-found" {
            continue; // not installed on this machine: nothing to report
        }
        let state = value("ActiveState");
        let level = match state.as_str() {
            "active" => Level::Ok,
            "activating" | "reloading" => Level::Warn,
            _ => Level::Down,
        };
        checks.push(check("admin-health-service", unit, level, state));
    }
    checks
}

/// Runs every check. Independent checks run at the same time, each with a short timeout.
pub async fn run_checks(state: &AppState, admin: &rustmail_core::admin::Admin) -> Vec<Check> {
    let mail = state.mail.clone();
    let imap = async {
        match &mail {
            Some(m) => {
                let c = &m.config;
                let result = timeout(PROBE_TIMEOUT, async {
                    let stream =
                        rustmail_imap::tls::connect(&c.clone(), &c.imap_host, c.imap_port).await?;
                    let mut line = String::new();
                    BufReader::new(stream)
                        .read_line(&mut line)
                        .await
                        .map_err(rustmail_imap::MailError::from)?;
                    Ok::<_, rustmail_imap::MailError>(
                        line.trim().chars().take(80).collect::<String>(),
                    )
                })
                .await;
                let subject = format!("{}:{}", c.imap_host, c.imap_port);
                match result {
                    Ok(Ok(line)) if line.contains("OK") => {
                        check("admin-health-imap", subject, Level::Ok, line)
                    }
                    Ok(Ok(line)) => check("admin-health-imap", subject, Level::Warn, line),
                    Ok(Err(error)) => {
                        check("admin-health-imap", subject, Level::Down, error.to_string())
                    }
                    Err(_) => check(
                        "admin-health-imap",
                        subject,
                        Level::Down,
                        "no answer within 3 seconds",
                    ),
                }
            }
            None => check(
                "admin-health-imap",
                "Dovecot",
                Level::Unchecked,
                "webmail settings are not configured",
            ),
        }
    };
    let smtp = async {
        match &mail {
            Some(m) => {
                probe_tcp(
                    "admin-health-smtp",
                    &m.config.smtp_host,
                    m.config.smtp_port,
                    "220",
                )
                .await
            }
            None => check(
                "admin-health-smtp",
                "Postfix",
                Level::Unchecked,
                "webmail settings are not configured",
            ),
        }
    };
    let vmail = std::env::var("VMAIL_BASE").unwrap_or_default();
    let quarantine = std::env::var("QUARANTINE_DIR").unwrap_or_default();
    let (db, imap, smtp, redis, clamd, amavis, queue, pipeline, mail_disk, quarantine_disk, units) = tokio::join!(
        database(state),
        imap,
        smtp,
        redis(),
        clamd(),
        amavis(),
        postfix_queue(),
        pipeline(state, admin),
        disk("mail storage", &vmail),
        disk("quarantine", &quarantine),
        systemd()
    );
    let mut all = vec![
        db,
        imap,
        smtp,
        redis,
        clamd,
        amavis,
        queue,
        pipeline,
        mail_disk,
        quarantine_disk,
    ];
    all.extend(units);
    all
}

/// The worst level among the checks that could be made (`Unchecked` never counts).
#[must_use]
pub fn overall(checks: &[Check]) -> Level {
    let mut worst = Level::Ok;
    for c in checks {
        match (c.level, worst) {
            (Level::Down, _) => return Level::Down,
            (Level::Warn, Level::Ok) => worst = Level::Warn,
            _ => {}
        }
    }
    worst
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(level: Level) -> Check {
        check("k", "s", level, "")
    }

    #[test]
    fn the_overall_status_is_the_worst_that_was_checked() {
        assert_eq!(overall(&[]), Level::Ok);
        assert_eq!(
            overall(&[c(Level::Ok), c(Level::Unchecked)]),
            Level::Ok,
            "unchecked is not a problem"
        );
        assert_eq!(overall(&[c(Level::Ok), c(Level::Warn)]), Level::Warn);
        assert_eq!(
            overall(&[c(Level::Warn), c(Level::Down), c(Level::Ok)]),
            Level::Down
        );
    }

    #[test]
    fn ages_are_readable() {
        assert_eq!(ago(5), "5 s");
        assert_eq!(ago(120), "2 min");
        assert_eq!(ago(7300), "2 h");
    }

    #[tokio::test]
    async fn a_closed_port_is_down_and_a_chatty_one_is_ok() {
        let down = probe_tcp("k", "127.0.0.1", 1, "220").await;
        assert_eq!(down.level, Level::Down);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let _ = s.write_all(b"220 hello\r\n").await;
        });
        let up = probe_tcp("k", "127.0.0.1", port, "220").await;
        assert_eq!((up.level, up.detail.as_str()), (Level::Ok, "220 hello"));
    }

    #[tokio::test]
    async fn missing_tools_are_unchecked_not_down() {
        assert_eq!(
            disk("x", "/definitely/not/here").await.level,
            Level::Unchecked
        );
        assert_eq!(clamd().await.level, Level::Unchecked);
        assert!(run("definitely-not-a-program", &[]).await.is_none());
    }
}
