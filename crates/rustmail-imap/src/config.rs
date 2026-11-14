//! Where the mail servers are and how to authenticate to them.
//!
//! The web server never sees a user's password after sign-in. Instead it logs in
//! to Dovecot as `<user>*<master>` with a shared secret (Dovecot's *master user*
//! feature), and does the same on Postfix's submission port. Dovecot restricts
//! the master login to configured networks (`MAIL_MASTER_ALLOW_NETS`).

use std::fmt;
use std::path::PathBuf;

use crate::error::{MailError, MailResult};

/// How strictly to check the servers' certificates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TlsMode {
    /// Verify against the public CA roots, plus an optional extra CA file.
    Verify {
        /// A PEM file with additional trusted certificates (a private CA, or the
        /// server's own certificate).
        ca_file: Option<PathBuf>,
    },
    /// Accept any certificate. For local development against a self-signed
    /// certificate only: it offers no protection against an attacker on the path.
    Insecure,
}

/// Connection settings for IMAP and SMTP submission.
#[derive(Clone)]
pub struct MailConfig {
    /// IMAP host.
    pub imap_host: String,
    /// IMAP port (implicit TLS, normally 993).
    pub imap_port: u16,
    /// SMTP submission host.
    pub smtp_host: String,
    /// SMTP submission port (STARTTLS, normally 587).
    pub smtp_port: u16,
    /// The name the servers' certificates are issued for. Defaults to the host.
    pub tls_server_name: Option<String>,
    /// Certificate checking.
    pub tls: TlsMode,
    /// Dovecot master user name.
    pub master_user: String,
    /// Dovecot master user password.
    pub master_password: String,
    /// Domain used in generated `Message-ID`s when the sender has none.
    pub helo_name: String,
}

impl fmt::Debug for MailConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MailConfig")
            .field(
                "imap",
                &format_args!("{}:{}", self.imap_host, self.imap_port),
            )
            .field(
                "smtp",
                &format_args!("{}:{}", self.smtp_host, self.smtp_port),
            )
            .field("tls", &self.tls)
            .field("master_user", &self.master_user)
            .field("master_password", &"<hidden>")
            .finish_non_exhaustive()
    }
}

impl MailConfig {
    /// Reads the settings from the environment.
    ///
    /// Returns `Ok(None)` when `MAIL_MASTER_PASSWORD` is not set: webmail is then
    /// simply not configured.
    ///
    /// | Variable | Default |
    /// |---|---|
    /// | `IMAP_HOST`, `IMAP_PORT` | `127.0.0.1`, `993` |
    /// | `SMTP_HOST`, `SMTP_PORT` | `IMAP_HOST`, `587` |
    /// | `MAIL_MASTER_USER` | `webmail` |
    /// | `MAIL_MASTER_PASSWORD` | (required to enable webmail) |
    /// | `MAIL_TLS_SERVER_NAME` | the host |
    /// | `MAIL_TLS_CA_FILE` | none |
    /// | `MAIL_TLS_INSECURE` | `0` |
    ///
    /// # Errors
    /// [`MailError::Config`] for unusable values.
    pub fn from_env() -> MailResult<Option<Self>> {
        let get = |key: &str| std::env::var(key).ok().filter(|v| !v.trim().is_empty());
        let Some(master_password) = get("MAIL_MASTER_PASSWORD") else {
            return Ok(None);
        };
        let port = |key: &str, default: u16| -> MailResult<u16> {
            match get(key) {
                None => Ok(default),
                Some(v) => v
                    .trim()
                    .parse()
                    .map_err(|_| MailError::Config(format!("{key} must be a port number"))),
            }
        };
        let imap_host = get("IMAP_HOST").unwrap_or_else(|| "127.0.0.1".to_owned());
        let insecure = matches!(
            get("MAIL_TLS_INSECURE").as_deref(),
            Some("1" | "true" | "yes")
        );
        let config = Self {
            smtp_host: get("SMTP_HOST").unwrap_or_else(|| imap_host.clone()),
            imap_host,
            imap_port: port("IMAP_PORT", 993)?,
            smtp_port: port("SMTP_PORT", 587)?,
            tls_server_name: get("MAIL_TLS_SERVER_NAME"),
            tls: if insecure {
                TlsMode::Insecure
            } else {
                TlsMode::Verify {
                    ca_file: get("MAIL_TLS_CA_FILE").map(PathBuf::from),
                }
            },
            master_user: get("MAIL_MASTER_USER").unwrap_or_else(|| "webmail".to_owned()),
            master_password,
            helo_name: get("MAIL_HELO_NAME").unwrap_or_else(|| "rustmail.local".to_owned()),
        };
        config.validate()?;
        Ok(Some(config))
    }

    /// Checks values that would break the protocols they are placed in.
    ///
    /// # Errors
    /// [`MailError::Config`] if the master user or password contain characters
    /// that are special in IMAP or SASL (`*`, NUL, line breaks) or are empty.
    pub fn validate(&self) -> MailResult<()> {
        let bad =
            |s: &str| s.is_empty() || s.chars().any(|c| c == '*' || c == '\0' || c.is_control());
        if bad(&self.master_user) || bad(&self.master_password) {
            return Err(MailError::Config(
                "the master user and password must not be empty or contain '*' or control characters".into(),
            ));
        }
        if self.helo_name.is_empty()
            || !self
                .helo_name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        {
            return Err(MailError::Config(
                "MAIL_HELO_NAME is not a valid host name".into(),
            ));
        }
        Ok(())
    }

    /// The name to check certificates against, for a connection to `host`.
    #[must_use]
    pub fn server_name<'a>(&'a self, host: &'a str) -> &'a str {
        self.tls_server_name.as_deref().unwrap_or(host)
    }

    /// The IMAP login name that acts as `user` through the master account.
    ///
    /// # Errors
    /// [`MailError::Invalid`] if `user` contains `*` or control characters, which
    /// could otherwise be used to confuse the master-user syntax.
    pub fn imap_login_for(&self, user: &str) -> MailResult<String> {
        if user.is_empty() || user.chars().any(|c| c == '*' || c.is_control()) {
            return Err(MailError::Invalid("invalid user name".into()));
        }
        Ok(format!("{user}*{}", self.master_user))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> MailConfig {
        MailConfig {
            imap_host: "mail.example.com".into(),
            imap_port: 993,
            smtp_host: "mail.example.com".into(),
            smtp_port: 587,
            tls_server_name: None,
            tls: TlsMode::Verify { ca_file: None },
            master_user: "webmail".into(),
            master_password: "s3cret".into(),
            helo_name: "rustmail.local".into(),
        }
    }

    #[test]
    fn the_master_login_has_the_documented_shape() {
        assert_eq!(
            config().imap_login_for("alice@example.com").unwrap(),
            "alice@example.com*webmail"
        );
    }

    #[test]
    fn users_cannot_smuggle_master_syntax() {
        for bad in ["", "a*b", "a\r\nb", "a\0b"] {
            assert!(config().imap_login_for(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn unsafe_master_credentials_are_refused() {
        for (user, password) in [
            ("we*bmail", "x"),
            ("webmail", ""),
            ("", "x"),
            ("webmail", "a\nb"),
        ] {
            let mut c = config();
            c.master_user = user.into();
            c.master_password = password.into();
            assert!(c.validate().is_err(), "{user:?} {password:?}");
        }
        assert!(config().validate().is_ok());
    }

    #[test]
    fn the_password_is_never_printed() {
        let shown = format!("{:?}", config());
        assert!(!shown.contains("s3cret"), "{shown}");
    }

    #[test]
    fn the_certificate_name_can_differ_from_the_host() {
        let mut c = config();
        assert_eq!(c.server_name("10.0.0.1"), "10.0.0.1");
        c.tls_server_name = Some("mail.example.com".into());
        assert_eq!(c.server_name("10.0.0.1"), "mail.example.com");
    }
}
