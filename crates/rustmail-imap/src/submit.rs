//! Sending a message through Postfix's submission port (587).
//!
//! Postfix gets the same treatment as any mail client would give it: STARTTLS,
//! then SASL `PLAIN`. The web server authenticates as the master user *on behalf
//! of* the signed-in user, so Postfix applies exactly the rules it applies to that
//! user: the sender must be one of their own addresses, and the message passes
//! through the virus and spam filters like any other.

use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use rustmail_core::smtp::{command, dot_stuff, reply, Reply};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::time::timeout;

use crate::compose::BuiltMessage;
use crate::config::MailConfig;
use crate::error::{MailError, MailResult};
use crate::tls;

/// How long a whole submission may take (large attachments take a while).
pub const SUBMIT_TIMEOUT: Duration = Duration::from_secs(180);

fn refused(what: &str, r: &Reply) -> MailError {
    // The server's own words are usually the most useful thing to show.
    MailError::Send(format!("{what}: {} {}", r.code, r.text))
}

fn expect(r: &Reply, ok: &[u16], what: &str) -> MailResult<()> {
    if ok.contains(&r.code) {
        Ok(())
    } else {
        Err(refused(what, r))
    }
}

fn io(error: &rustmail_core::Error) -> MailError {
    MailError::Send(error.to_string())
}

/// The SASL `PLAIN` initial response that logs in as `user` through the master account.
#[must_use]
pub fn plain_initial_response(user: &str, master_user: &str, master_password: &str) -> String {
    STANDARD.encode(format!("{user}\0{master_user}\0{master_password}"))
}

/// Submits `message` for `user`. The envelope sender is `sender` (the user's own
/// address); the recipients are `message.recipients`.
///
/// Nothing is sent unless every recipient is accepted.
///
/// # Errors
/// [`MailError::Send`] with the server's explanation, or connection/TLS errors.
pub async fn send(config: &MailConfig, user: &str, sender: &str, message: &BuiltMessage) -> MailResult<()> {
    if sender.chars().any(|c| c.is_control() || c == '<' || c == '>' || c.is_whitespace()) {
        return Err(MailError::Invalid("invalid sender".into()));
    }
    if message.recipients.is_empty() {
        return Err(MailError::Invalid("add at least one recipient".into()));
    }
    // Both are placed in a SASL string split on NUL; the config checks the master side.
    if user.contains('\0') {
        return Err(MailError::Invalid("invalid user name".into()));
    }
    timeout(SUBMIT_TIMEOUT, dialogue(config, user, sender, message))
        .await
        .map_err(|_| MailError::Send("the mail server took too long".into()))?
}

async fn dialogue(config: &MailConfig, user: &str, sender: &str, message: &BuiltMessage) -> MailResult<()> {
    let host = &config.smtp_host;
    let mut plain = BufReader::new(tls::tcp(host, config.smtp_port).await?);

    expect(&reply(&mut plain).await.map_err(|e| io(&e))?, &[220], "the server did not greet us")?;
    command(&mut plain, &format!("EHLO {}\r\n", config.helo_name)).await.map_err(|e| io(&e))?;
    let ehlo = reply(&mut plain).await.map_err(|e| io(&e))?;
    expect(&ehlo, &[250], "EHLO")?;
    if !ehlo.text.to_ascii_uppercase().contains("STARTTLS") {
        // Never send the master credentials over an unencrypted connection.
        return Err(MailError::Tls("the submission server does not offer STARTTLS".into()));
    }
    command(&mut plain, "STARTTLS\r\n").await.map_err(|e| io(&e))?;
    expect(&reply(&mut plain).await.map_err(|e| io(&e))?, &[220], "STARTTLS")?;

    let secure = tls::upgrade(config, host, plain.into_inner()).await?;
    let mut conn = BufReader::new(secure);
    command(&mut conn, &format!("EHLO {}\r\n", config.helo_name)).await.map_err(|e| io(&e))?;
    expect(&reply(&mut conn).await.map_err(|e| io(&e))?, &[250], "EHLO after STARTTLS")?;

    let auth = plain_initial_response(user, &config.master_user, &config.master_password);
    command(&mut conn, &format!("AUTH PLAIN {auth}\r\n")).await.map_err(|e| io(&e))?;
    let auth_reply = reply(&mut conn).await.map_err(|e| io(&e))?;
    if auth_reply.code != 235 {
        tracing::warn!(code = auth_reply.code, "submission login refused");
        return Err(MailError::Send("the mail server refused the login".into()));
    }

    command(&mut conn, &format!("MAIL FROM:<{sender}>\r\n")).await.map_err(|e| io(&e))?;
    expect(&reply(&mut conn).await.map_err(|e| io(&e))?, &[250], "the sender address was refused")?;

    for recipient in &message.recipients {
        command(&mut conn, &format!("RCPT TO:<{recipient}>\r\n")).await.map_err(|e| io(&e))?;
        let r = reply(&mut conn).await.map_err(|e| io(&e))?;
        if !matches!(r.code, 250 | 251) {
            let _ = command(&mut conn, "RSET\r\nQUIT\r\n").await;
            return Err(refused(&format!("{recipient} was refused"), &r));
        }
    }

    command(&mut conn, "DATA\r\n").await.map_err(|e| io(&e))?;
    expect(&reply(&mut conn).await.map_err(|e| io(&e))?, &[354], "DATA")?;
    conn.write_all(&dot_stuff(&message.bytes))
        .await
        .map_err(|e| MailError::Send(e.to_string()))?;
    conn.write_all(b".\r\n").await.map_err(|e| MailError::Send(e.to_string()))?;
    conn.flush().await.map_err(|e| MailError::Send(e.to_string()))?;
    expect(&reply(&mut conn).await.map_err(|e| io(&e))?, &[250], "the message was refused")?;
    // Queued. A failure to say goodbye changes nothing.
    let _ = command(&mut conn, "QUIT\r\n").await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sasl_response_names_the_user_then_the_master() {
        let encoded = plain_initial_response("alice@example.com", "webmail", "pw");
        let decoded = STANDARD.decode(encoded).unwrap();
        assert_eq!(decoded, b"alice@example.com\0webmail\0pw");
    }

    #[tokio::test]
    async fn obviously_bad_input_is_refused_before_connecting() {
        let config = MailConfig {
            imap_host: "127.0.0.1".into(),
            imap_port: 1,
            smtp_host: "127.0.0.1".into(),
            smtp_port: 1,
            tls_server_name: None,
            tls: crate::config::TlsMode::Insecure,
            master_user: "webmail".into(),
            master_password: "x".into(),
            helo_name: "rustmail.local".into(),
        };
        let message = BuiltMessage { bytes: b"x".to_vec(), message_id: "i".into(), recipients: vec![] };
        assert!(matches!(send(&config, "a@b.co", "a@b.co", &message).await, Err(MailError::Invalid(_))));
        let message = BuiltMessage { recipients: vec!["r@x.co".into()], ..message };
        assert!(matches!(send(&config, "a@b.co", "a@b.co>\r\nRCPT", &message).await, Err(MailError::Invalid(_))));
        // Valid input but nothing listening: a connection error, not a panic or a hang.
        assert!(matches!(send(&config, "a@b.co", "a@b.co", &message).await, Err(MailError::Connect(_))));
    }
}
