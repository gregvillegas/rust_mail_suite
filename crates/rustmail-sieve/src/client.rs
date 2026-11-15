//! A ManageSieve session: capability, STARTTLS, login and the script commands.

use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use rustmail_imap::tls;
use rustmail_imap::MailConfig;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::client::TlsStream;

use crate::error::{tidy, Quoted, SieveError, SieveResult};
use crate::protocol::{read_response, Response, Status, Token};

/// How long one command may take.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);

/// Where the ManageSieve server is.
#[derive(Clone)]
pub struct SieveConfig {
    /// Host name or address.
    pub host: String,
    /// Port (4190 by default).
    pub port: u16,
    /// The mail settings: certificate checking and the master login.
    pub mail: MailConfig,
}

impl SieveConfig {
    /// Same host as IMAP unless `SIEVE_HOST` is set; port `SIEVE_PORT` or 4190.
    ///
    /// # Errors
    /// [`SieveError::Invalid`] if `SIEVE_PORT` is not a port number.
    pub fn from_mail(mail: &MailConfig) -> SieveResult<Self> {
        let host = std::env::var("SIEVE_HOST")
            .ok()
            .filter(|h| !h.trim().is_empty())
            .unwrap_or_else(|| mail.imap_host.clone());
        let port = match std::env::var("SIEVE_PORT") {
            Ok(p) if !p.trim().is_empty() => p
                .trim()
                .parse()
                .map_err(|_| SieveError::Invalid("SIEVE_PORT must be a port number".into()))?,
            _ => 4190,
        };
        Ok(Self {
            host,
            port,
            mail: mail.clone(),
        })
    }
}

/// A script on the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptInfo {
    /// The script's name.
    pub name: String,
    /// Whether it is the one Dovecot runs.
    pub active: bool,
}

/// A logged-in ManageSieve session over any stream.
pub struct Session<S> {
    io: BufReader<S>,
}

/// A session over TLS to a real server.
pub type TlsSession = Session<TlsStream<TcpStream>>;

/// Script names must be simple: letters, digits, `-`, `_`, `.`.
fn check_name(name: &str) -> SieveResult<()> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(SieveError::Invalid("invalid script name".into()));
    }
    Ok(())
}

fn has_capability(r: &Response, name: &str) -> bool {
    r.lines
        .iter()
        .any(|l| matches!(l.first(), Some(Token::Str(s)) if s.eq_ignore_ascii_case(name)))
}

/// Turns a non-OK response into the right error.
fn refusal(r: &Response, script: bool) -> SieveError {
    let text = tidy(&r.text);
    match (r.status, script) {
        (Status::Ok, _) => SieveError::Protocol("unexpected OK".into()),
        (Status::Bye, _) => SieveError::Connect(format!("the server ended the session: {text}")),
        (Status::No, true) => SieveError::Rejected(text),
        (Status::No, false) => SieveError::Refused(text),
    }
}

impl TlsSession {
    /// Connects, upgrades to TLS, and logs in as `user` through the master account.
    ///
    /// # Errors
    /// [`SieveError`] if the server is unreachable, offers no STARTTLS, or refuses the login.
    pub async fn connect(config: &SieveConfig, user: &str) -> SieveResult<Self> {
        let login = config.mail.imap_login_for(user)?;
        let tcp = tls::tcp(&config.host, config.port).await?;
        let mut plain = BufReader::new(tcp);
        let greeting = timeout(COMMAND_TIMEOUT, read_response(&mut plain))
            .await
            .map_err(|_| SieveError::Connect("no greeting from the server".into()))??;
        if greeting.status != Status::Ok {
            return Err(refusal(&greeting, false));
        }
        // Credentials only ever travel inside TLS.
        if !has_capability(&greeting, "STARTTLS") {
            return Err(SieveError::Tls("the server does not offer STARTTLS".into()));
        }
        plain.get_mut().write_all(b"STARTTLS\r\n").await?;
        let go = timeout(COMMAND_TIMEOUT, read_response(&mut plain))
            .await
            .map_err(|_| SieveError::Connect("STARTTLS timed out".into()))??;
        if go.status != Status::Ok {
            return Err(SieveError::Tls(tidy(&go.text)));
        }
        let tcp = plain.into_inner();
        let secure = tls::upgrade(&config.mail, &config.host, tcp).await?;
        let mut session = Session {
            io: BufReader::new(secure),
        };
        // After TLS the server repeats its capabilities.
        let caps = session.read().await?;
        if caps.status != Status::Ok {
            return Err(refusal(&caps, false));
        }
        session
            .authenticate(&login, &config.mail.master_password)
            .await?;
        Ok(session)
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> Session<S> {
    /// Wraps an already-secured stream whose greeting has been read. Used by tests.
    #[must_use]
    pub fn from_stream(stream: S) -> Self {
        Self {
            io: BufReader::new(stream),
        }
    }

    async fn read(&mut self) -> SieveResult<Response> {
        timeout(COMMAND_TIMEOUT, read_response(&mut self.io))
            .await
            .map_err(|_| SieveError::Connect("the server did not answer in time".into()))?
    }

    async fn send(&mut self, bytes: &[u8]) -> SieveResult<()> {
        timeout(COMMAND_TIMEOUT, async {
            self.io.get_mut().write_all(bytes).await?;
            self.io.get_mut().flush().await
        })
        .await
        .map_err(|_| SieveError::Connect("the server did not accept data in time".into()))??;
        Ok(())
    }

    /// Logs in with SASL PLAIN. `login` may be `user*master`.
    ///
    /// # Errors
    /// [`SieveError::Auth`] when refused.
    pub async fn authenticate(&mut self, login: &str, password: &str) -> SieveResult<()> {
        let blob = STANDARD.encode(format!("\0{login}\0{password}"));
        self.send(format!("AUTHENTICATE \"PLAIN\" \"{blob}\"\r\n").as_bytes())
            .await?;
        let r = self.read().await?;
        match r.status {
            Status::Ok => Ok(()),
            Status::No => Err(SieveError::Auth),
            Status::Bye => Err(refusal(&r, false)),
        }
    }

    /// Lists the user's scripts.
    ///
    /// # Errors
    /// [`SieveError`] on a refusal or a broken connection.
    pub async fn list(&mut self) -> SieveResult<Vec<ScriptInfo>> {
        self.send(b"LISTSCRIPTS\r\n").await?;
        let r = self.read().await?;
        if r.status != Status::Ok {
            return Err(refusal(&r, false));
        }
        Ok(r.lines
            .iter()
            .filter_map(|l| match l.as_slice() {
                [Token::Str(name), rest @ ..] => Some(ScriptInfo {
                    name: name.clone(),
                    active: rest
                        .iter()
                        .any(|t| matches!(t, Token::Atom(a) if a.eq_ignore_ascii_case("ACTIVE"))),
                }),
                _ => None,
            })
            .collect())
    }

    /// Fetches a script's text, or `None` if there is no such script.
    ///
    /// # Errors
    /// [`SieveError`] on a refusal other than "no such script".
    pub async fn get(&mut self, name: &str) -> SieveResult<Option<String>> {
        check_name(name)?;
        self.send(format!("GETSCRIPT {}\r\n", Quoted(name)).as_bytes())
            .await?;
        let r = self.read().await?;
        match r.status {
            Status::Ok => Ok(r.lines.into_iter().find_map(|l| {
                if let [Token::Str(s)] = l.as_slice() {
                    Some(s.clone())
                } else {
                    None
                }
            })),
            Status::No => Ok(None),
            Status::Bye => Err(refusal(&r, false)),
        }
    }

    async fn script_command(
        &mut self,
        verb: &str,
        name: Option<&str>,
        content: &str,
    ) -> SieveResult<()> {
        let mut cmd = format!("{verb} ");
        if let Some(name) = name {
            check_name(name)?;
            cmd.push_str(&format!("{} ", Quoted(name)));
        }
        // A non-synchronising literal: the length is in bytes, not characters.
        cmd.push_str(&format!("{{{}+}}\r\n{content}\r\n", content.len()));
        self.send(cmd.as_bytes()).await?;
        let r = self.read().await?;
        if r.status == Status::Ok {
            Ok(())
        } else {
            Err(refusal(&r, true))
        }
    }

    /// Asks the server to check a script without storing it.
    ///
    /// # Errors
    /// [`SieveError::Rejected`] with the server's explanation when the script is wrong.
    pub async fn check(&mut self, content: &str) -> SieveResult<()> {
        self.script_command("CHECKSCRIPT", None, content).await
    }

    /// Stores a script (replacing one of the same name). The server rejects scripts it cannot compile.
    ///
    /// # Errors
    /// [`SieveError::Rejected`] with the server's explanation when the script is wrong.
    pub async fn put(&mut self, name: &str, content: &str) -> SieveResult<()> {
        self.script_command("PUTSCRIPT", Some(name), content).await
    }

    /// Makes `name` the script Dovecot runs.
    ///
    /// # Errors
    /// [`SieveError::Refused`] if there is no such script.
    pub async fn activate(&mut self, name: &str) -> SieveResult<()> {
        check_name(name)?;
        self.send(format!("SETACTIVE {}\r\n", Quoted(name)).as_bytes())
            .await?;
        let r = self.read().await?;
        if r.status == Status::Ok {
            Ok(())
        } else {
            Err(refusal(&r, false))
        }
    }

    /// Stops running any script (`SETACTIVE ""`). The scripts stay stored.
    ///
    /// # Errors
    /// [`SieveError::Refused`] when the server says no.
    pub async fn deactivate(&mut self) -> SieveResult<()> {
        self.send(b"SETACTIVE \"\"\r\n").await?;
        let r = self.read().await?;
        if r.status == Status::Ok {
            Ok(())
        } else {
            Err(refusal(&r, false))
        }
    }

    /// Deletes a script (an active script must be deactivated first).
    ///
    /// # Errors
    /// [`SieveError::Refused`] when the server says no.
    pub async fn delete(&mut self, name: &str) -> SieveResult<()> {
        check_name(name)?;
        self.send(format!("DELETESCRIPT {}\r\n", Quoted(name)).as_bytes())
            .await?;
        let r = self.read().await?;
        if r.status == Status::Ok {
            Ok(())
        } else {
            Err(refusal(&r, false))
        }
    }

    /// Ends the session politely. Errors are ignored: the work is already done.
    pub async fn logout(mut self) {
        if self.send(b"LOGOUT\r\n").await.is_ok() {
            let _ = self.read().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{duplex, AsyncBufReadExt};

    /// Runs `server` against the client end of an in-memory pipe.
    async fn with_server<F, Fut>(server: F) -> Session<tokio::io::DuplexStream>
    where
        F: FnOnce(BufReader<tokio::io::DuplexStream>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send,
    {
        let (client, srv) = duplex(64 * 1024);
        tokio::spawn(async move { server(BufReader::new(srv)).await });
        Session::from_stream(client)
    }

    async fn line(r: &mut BufReader<tokio::io::DuplexStream>) -> String {
        let mut s = String::new();
        r.read_line(&mut s).await.unwrap();
        s.trim_end().to_owned()
    }

    #[tokio::test]
    async fn lists_scripts_and_marks_the_active_one() {
        let mut s = with_server(|mut srv| async move {
            assert_eq!(line(&mut srv).await, "LISTSCRIPTS");
            srv.get_mut().write_all(b"\"rustmail\" ACTIVE\r\n\"rustmail-filters\"\r\nOK \"Listscripts completed.\"\r\n").await.unwrap();
        })
        .await;
        let scripts = s.list().await.unwrap();
        assert_eq!(
            scripts,
            vec![
                ScriptInfo {
                    name: "rustmail".into(),
                    active: true
                },
                ScriptInfo {
                    name: "rustmail-filters".into(),
                    active: false
                }
            ]
        );
    }

    #[tokio::test]
    async fn put_sends_the_byte_length_not_the_character_count() {
        let script = "# Größe\nkeep;\n";
        let mut s = with_server(move |mut srv| async move {
            let head = line(&mut srv).await;
            assert_eq!(
                head,
                format!("PUTSCRIPT \"rustmail-filters\" {{{}+}}", script.len())
            );
            assert!(script.len() > script.chars().count());
            let mut body = vec![0; script.len()];
            tokio::io::AsyncReadExt::read_exact(&mut srv, &mut body)
                .await
                .unwrap();
            assert_eq!(String::from_utf8(body).unwrap(), script);
            srv.get_mut()
                .write_all(b"OK \"Putscript completed.\"\r\n")
                .await
                .unwrap();
        })
        .await;
        s.put("rustmail-filters", script).await.unwrap();
    }

    #[tokio::test]
    async fn a_rejected_script_returns_the_servers_explanation() {
        let mut s = with_server(|mut srv| async move {
            let _ = line(&mut srv).await;
            srv.get_mut().write_all(b"NO \"line 2: error: unknown command 'fileintoo' (only reported once at first occurrence).\"\r\n").await.unwrap();
        })
        .await;
        let err = s.put("rustmail-filters", "x\n").await.unwrap_err();
        assert!(err.is_script_error());
        assert!(
            err.to_string().contains("line 2") && err.to_string().contains("fileintoo"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn get_returns_the_text_or_none() {
        let mut s = with_server(|mut srv| async move {
            assert_eq!(line(&mut srv).await, "GETSCRIPT \"a\"");
            srv.get_mut()
                .write_all(b"{7}\r\nkeep;\r\n\r\nOK\r\n")
                .await
                .unwrap();
            assert_eq!(line(&mut srv).await, "GETSCRIPT \"b\"");
            srv.get_mut()
                .write_all(b"NO (NONEXISTENT) \"There is no script by that name\"\r\n")
                .await
                .unwrap();
        })
        .await;
        assert_eq!(s.get("a").await.unwrap().as_deref(), Some("keep;\r\n"));
        assert_eq!(s.get("b").await.unwrap(), None);
    }

    #[tokio::test]
    async fn login_failure_is_reported_without_the_servers_text() {
        let mut s = with_server(|mut srv| async move {
            let head = line(&mut srv).await;
            assert!(head.starts_with("AUTHENTICATE \"PLAIN\" \""), "{head}");
            let blob = head.rsplit('"').nth(1).unwrap().to_owned();
            assert_eq!(STANDARD.decode(blob).unwrap(), b"\0alice*webmail\0pw");
            srv.get_mut()
                .write_all(b"NO \"Authentication failed.\"\r\n")
                .await
                .unwrap();
        })
        .await;
        assert!(matches!(
            s.authenticate("alice*webmail", "pw").await,
            Err(SieveError::Auth)
        ));
    }

    #[tokio::test]
    async fn names_cannot_smuggle_commands() {
        let mut s = with_server(|_srv| async {}).await;
        for bad in [
            "",
            "a b",
            "x\"\r\nDELETESCRIPT \"y",
            "../x",
            &"a".repeat(65),
        ] {
            assert!(
                matches!(s.get(bad).await, Err(SieveError::Invalid(_))),
                "{bad:?}"
            );
            assert!(s.put(bad, "keep;").await.is_err());
        }
    }

    #[tokio::test]
    async fn deactivating_sends_an_empty_name() {
        let mut s = with_server(|mut srv| async move {
            assert_eq!(line(&mut srv).await, "SETACTIVE \"\"");
            srv.get_mut()
                .write_all(b"OK \"Setactive completed.\"\r\n")
                .await
                .unwrap();
        })
        .await;
        s.deactivate().await.unwrap();
    }

    #[tokio::test]
    async fn a_dead_server_is_an_error_not_a_hang() {
        let (client, srv) = duplex(1024);
        drop(srv);
        let mut s = Session::from_stream(client);
        assert!(s.list().await.is_err());
    }
}
