//! Bulk user import from CSV.
//!
//! Columns (a header row is required, in any order): `email` (required), `name`,
//! `role` (`user`, `domain_admin`, `super_admin`), `quota_mb` and `password`. A
//! missing password means one is generated; generated passwords are returned once
//! in the result and stored nowhere in clear.
//!
//! It is a two-step operation on purpose. [`AdminService::preview_import`] checks
//! every row against the database without changing anything, so mistakes are found
//! before any account exists. [`AdminService::apply_import`] repeats the checks
//! (the data may have changed since the preview) and then creates each valid row
//! independently: one bad row never blocks the others.

use std::collections::{HashMap, HashSet};

use serde_json::json;
use uuid::Uuid;

use super::csv;
use super::users::{clean_display_name, NewAccount};
use super::{Admin, AdminService};
use crate::domain::validation::split_email;
use crate::domain::{AuditOutcome, UserRole};
use crate::error::{Error, Result};

/// Highest quota accepted in the file, in megabytes (about 10 TB).
pub const MAX_QUOTA_MB: i64 = 10_000_000;

/// A row that passed the checks.
#[derive(Clone, PartialEq)]
pub struct ImportRow {
    /// Line number in the file (the header is line 1).
    pub line: usize,
    /// Normalised address.
    pub email: String,
    /// Display name.
    pub name: String,
    /// Role.
    pub role: UserRole,
    /// Quota in bytes; `None` inherits the domain default.
    pub quota_bytes: Option<i64>,
    /// A password from the file, if any.
    pub password: Option<String>,
}

impl std::fmt::Debug for ImportRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportRow")
            .field("line", &self.line)
            .field("email", &self.email)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
    }
}

/// A row that did not pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportProblem {
    /// Line number in the file.
    pub line: usize,
    /// The address as written.
    pub email: String,
    /// What is wrong, for the administrator.
    pub message: String,
}

/// The result of checking a file.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportPreview {
    /// Rows that would be created.
    pub valid: Vec<ImportRow>,
    /// Rows that would be skipped, and why.
    pub problems: Vec<ImportProblem>,
}

/// What happened to one row when applying.
#[derive(Clone, PartialEq, Eq)]
pub struct ImportResult {
    /// Line number in the file.
    pub line: usize,
    /// The address.
    pub email: String,
    /// Whether the account was created.
    pub created: bool,
    /// The generated password, for created accounts that had none in the file.
    pub generated_password: Option<String>,
    /// Why it was not created.
    pub error: Option<String>,
}

impl std::fmt::Debug for ImportResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportResult")
            .field("line", &self.line)
            .field("email", &self.email)
            .field("created", &self.created)
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

struct Columns {
    email: usize,
    name: Option<usize>,
    role: Option<usize>,
    quota: Option<usize>,
    password: Option<usize>,
}

fn read_header(header: &[String]) -> Result<Columns> {
    let mut cols = Columns {
        email: usize::MAX,
        name: None,
        role: None,
        quota: None,
        password: None,
    };
    for (i, raw) in header.iter().enumerate() {
        let slot = match raw.trim().to_lowercase().replace([' ', '-'], "_").as_str() {
            "email" | "e_mail" | "address" => Some(0),
            "name" | "display_name" | "full_name" => Some(1),
            "role" => Some(2),
            "quota_mb" | "quota" => Some(3),
            "password" => Some(4),
            other => {
                return Err(Error::Validation(format!(
                    "unknown column '{}'; use email, name, role, quota_mb, password",
                    other.chars().take(40).collect::<String>()
                )))
            }
        };
        match slot {
            Some(0) => cols.email = i,
            Some(1) => cols.name = Some(i),
            Some(2) => cols.role = Some(i),
            Some(3) => cols.quota = Some(i),
            Some(4) => cols.password = Some(i),
            _ => {}
        }
    }
    if cols.email == usize::MAX {
        return Err(Error::Validation(
            "the first row must name the columns, including 'email'".into(),
        ));
    }
    Ok(cols)
}

/// A row from the file before any database checks.
struct Parsed {
    line: usize,
    email_raw: String,
    outcome: std::result::Result<ImportRow, String>,
}

fn parse_rows(text: &str) -> Result<Vec<Parsed>> {
    let table = csv::parse(text)?;
    let mut rows = table.into_iter();
    let header = rows
        .next()
        .ok_or_else(|| Error::Validation("the file is empty".into()))?;
    let cols = read_header(&header)?;
    let cell = |row: &[String], i: Option<usize>| {
        i.and_then(|i| row.get(i))
            .map(|s| s.trim().to_owned())
            .unwrap_or_default()
    };
    Ok(rows
        .enumerate()
        .map(|(n, row)| {
            // Line numbers are counted in rows, which is what a spreadsheet shows.
            let line = n + 2;
            let email_raw = row
                .get(cols.email)
                .map(|s| s.trim().to_owned())
                .unwrap_or_default();
            let outcome = (|| {
                let email = crate::domain::validation::normalize_email(&email_raw)
                    .map_err(|e| e.to_string())?;
                let name = clean_display_name(&cell(&row, cols.name)).map_err(|e| e.to_string())?;
                let role = match cell(&row, cols.role)
                    .to_lowercase()
                    .replace('-', "_")
                    .as_str()
                {
                    "" | "user" => UserRole::User,
                    "domain_admin" | "domain_administrator" => UserRole::DomainAdmin,
                    "super_admin" | "super_administrator" => UserRole::SuperAdmin,
                    other => {
                        return Err(format!(
                            "unknown role '{}'",
                            other.chars().take(30).collect::<String>()
                        ))
                    }
                };
                let quota = cell(&row, cols.quota);
                let quota_bytes = if quota.is_empty() {
                    None
                } else {
                    let mb: i64 = quota.parse().map_err(|_| {
                        format!(
                            "quota_mb '{}' is not a whole number",
                            quota.chars().take(20).collect::<String>()
                        )
                    })?;
                    if !(0..=MAX_QUOTA_MB).contains(&mb) {
                        return Err(format!("quota_mb must be between 0 and {MAX_QUOTA_MB}"));
                    }
                    Some(mb * 1024 * 1024)
                };
                let password = Some(cell(&row, cols.password)).filter(|p| !p.is_empty());
                if let Some(p) = &password {
                    crate::password::check_password_length(p).map_err(|e| e.to_string())?;
                }
                Ok(ImportRow {
                    line,
                    email,
                    name,
                    role,
                    quota_bytes,
                    password,
                })
            })();
            Parsed {
                line,
                email_raw,
                outcome,
            }
        })
        .collect())
}

impl AdminService {
    /// Checks a CSV file without creating anything.
    ///
    /// # Errors
    /// [`Error::Validation`] if the file cannot be read as CSV at all (too large,
    /// no header, unknown columns); problems with single rows are reported in the
    /// result instead.
    pub async fn preview_import(&self, admin: &Admin, text: &str) -> Result<ImportPreview> {
        let parsed = parse_rows(text)?;
        let mut valid = Vec::new();
        let mut problems = Vec::new();
        let mut candidates = Vec::new();
        for p in parsed {
            match p.outcome {
                Ok(row) => candidates.push(row),
                Err(message) => problems.push(ImportProblem {
                    line: p.line,
                    email: p.email_raw,
                    message,
                }),
            }
        }

        let emails: Vec<String> = candidates.iter().map(|r| r.email.clone()).collect();
        let used: HashSet<String> = sqlx::query_scalar!(
            r#"SELECT x AS "x!" FROM (
                   SELECT email AS x FROM users WHERE email = ANY($1)
                   UNION SELECT address FROM aliases WHERE address = ANY($1)
                   UNION SELECT address FROM groups WHERE address = ANY($1) AND address IS NOT NULL) t"#,
            &emails
        )
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .collect();
        let names: Vec<String> = candidates
            .iter()
            .filter_map(|r| split_email(&r.email).ok().map(|(_, d)| d))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        struct Dom {
            id: Uuid,
            active: bool,
            room: Option<i64>,
        }
        let domains: HashMap<String, Dom> = sqlx::query!(
            r#"SELECT d.name, d.id, d.status::text AS "status!", d.max_mailboxes,
                      (SELECT count(*) FROM users u WHERE u.domain_id = d.id) AS "users!"
               FROM domains d WHERE d.name = ANY($1)"#,
            &names
        )
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .map(|r| {
            (
                r.name,
                Dom {
                    id: r.id,
                    active: r.status == "active",
                    room: r.max_mailboxes.map(|m| i64::from(m) - r.users),
                },
            )
        })
        .collect();

        let mut seen: HashSet<String> = HashSet::new();
        let mut taken_room: HashMap<Uuid, i64> = HashMap::new();
        for row in candidates {
            let problem = |message: String| ImportProblem {
                line: row.line,
                email: row.email.clone(),
                message,
            };
            let Ok((_, domain_name)) = split_email(&row.email) else {
                problems.push(problem("not a valid address".into()));
                continue;
            };
            let Some(domain) = domains.get(&domain_name) else {
                problems.push(problem(format!("the domain {domain_name} does not exist")));
                continue;
            };
            if !admin.scope.allows(domain.id) {
                problems.push(problem(format!("you may not add users to {domain_name}")));
                continue;
            }
            if !domain.active {
                problems.push(problem(format!("{domain_name} is not active")));
                continue;
            }
            if !admin.is_super() && row.role != UserRole::User {
                problems.push(problem(
                    "only a super administrator may create administrators".into(),
                ));
                continue;
            }
            if used.contains(&row.email) {
                problems.push(problem(
                    "this address already exists (as a mailbox, alias or group)".into(),
                ));
                continue;
            }
            if !seen.insert(row.email.clone()) {
                problems.push(problem("this address appears twice in the file".into()));
                continue;
            }
            if let Some(room) = domain.room {
                let taken = taken_room.entry(domain.id).or_insert(0);
                if *taken >= room {
                    problems.push(problem(format!(
                        "{domain_name} has reached its mailbox limit"
                    )));
                    continue;
                }
                *taken += 1;
            }
            valid.push(row);
        }
        problems.sort_by_key(|p| p.line);
        Ok(ImportPreview { valid, problems })
    }

    /// Creates the accounts of every valid row; rows that fail are reported, not fatal.
    ///
    /// # Errors
    /// [`Error::Validation`] if the file cannot be read as CSV at all.
    pub async fn apply_import(&self, admin: &Admin, text: &str) -> Result<Vec<ImportResult>> {
        let preview = self.preview_import(admin, text).await?;
        let mut results: Vec<ImportResult> = preview
            .problems
            .iter()
            .map(|p| ImportResult {
                line: p.line,
                email: p.email.clone(),
                created: false,
                generated_password: None,
                error: Some(p.message.clone()),
            })
            .collect();
        for row in preview.valid {
            let outcome = self
                .create_user(
                    admin,
                    NewAccount {
                        email: row.email.clone(),
                        display_name: row.name.clone(),
                        role: row.role,
                        quota_bytes: row.quota_bytes,
                        password: row.password.clone(),
                    },
                )
                .await;
            results.push(match outcome {
                Ok(created) => ImportResult {
                    line: row.line,
                    email: row.email,
                    created: true,
                    generated_password: created.generated_password,
                    error: None,
                },
                Err(error) => ImportResult {
                    line: row.line,
                    email: row.email,
                    created: false,
                    generated_password: None,
                    error: Some(error.to_string()),
                },
            });
        }
        results.sort_by_key(|r| r.line);
        let created = results.iter().filter(|r| r.created).count();
        self.audit(
            admin,
            "user.import",
            None,
            admin.scope.domain_filter(),
            AuditOutcome::Success,
            json!({"created": created, "failed": results.len() - created}),
        )
        .await;
        Ok(results)
    }
}

/// The import result as CSV, including generated passwords. Treat the file as a secret.
#[must_use]
pub fn results_csv(results: &[ImportResult]) -> String {
    let mut out = csv::write_row(&["line", "email", "result", "password", "error"]);
    for r in results {
        out.push_str(&csv::write_row(&[
            r.line.to_string(),
            r.email.clone(),
            if r.created {
                "created".to_owned()
            } else {
                "failed".to_owned()
            },
            r.generated_password.clone().unwrap_or_default(),
            r.error.clone().unwrap_or_default(),
        ]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_are_flexible_but_strict_about_unknowns() {
        let cols = read_header(&["Email".into(), "Display Name".into(), "quota".into()]).unwrap();
        assert_eq!((cols.email, cols.name, cols.quota), (0, Some(1), Some(2)));
        assert!(
            read_header(&["name".into(), "role".into()]).is_err(),
            "email is required"
        );
        assert!(read_header(&["email".into(), "shoe_size".into()]).is_err());
    }

    #[test]
    fn rows_are_parsed_and_checked_for_syntax() {
        let csv = "email,name,role,quota_mb,password\n\
                   Anna@Example.com,Anna A,user,100,\n\
                   bob@example.com,Bob,domain-admin,,long-enough-password\n\
                   bad,Nobody,user,,\n\
                   eve@example.com,Eve,wizard,,\n\
                   fay@example.com,Fay,user,-5,\n\
                   gus@example.com,Gus,user,abc,\n\
                   hal@example.com,Hal,user,,short\n";
        let rows = parse_rows(csv).unwrap();
        assert_eq!(rows.len(), 7);
        let anna = rows[0].outcome.as_ref().unwrap();
        assert_eq!(
            (anna.email.as_str(), anna.quota_bytes, anna.line),
            ("anna@example.com", Some(100 * 1024 * 1024), 2)
        );
        assert_eq!(
            rows[1].outcome.as_ref().unwrap().role,
            UserRole::DomainAdmin
        );
        for (index, row) in rows.iter().enumerate().skip(2) {
            assert!(row.outcome.is_err(), "row {index}");
        }
        assert!(rows[3]
            .outcome
            .as_ref()
            .unwrap_err()
            .contains("unknown role"));
    }

    #[test]
    fn passwords_never_appear_in_debug_output() {
        let row = ImportRow {
            line: 2,
            email: "a@b.co".into(),
            name: String::new(),
            role: UserRole::User,
            quota_bytes: None,
            password: Some("hunter2-hunter2".into()),
        };
        assert!(!format!("{row:?}").contains("hunter2"));
        let result = ImportResult {
            line: 2,
            email: "a@b.co".into(),
            created: true,
            generated_password: Some("s3cretvalue".into()),
            error: None,
        };
        assert!(!format!("{result:?}").contains("s3cretvalue"));
    }

    #[test]
    fn the_result_file_neutralises_formulas() {
        let results = vec![ImportResult {
            line: 2,
            email: "=cmd@x.co".into(),
            created: false,
            generated_password: None,
            error: Some("+bad".into()),
        }];
        let out = results_csv(&results);
        assert!(out.contains("'=cmd@x.co") && out.contains("'+bad"), "{out}");
    }
}
