//! A deliberately tiny SMTP client, used to hand a released message back to
//! Postfix's re-entry port.
//!
//! It speaks just enough SMTP for one message to one recipient over a plain,
//! trusted connection (loopback or a private network). It is not a general mail
//! client: no TLS, no authentication, no pipelining.

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::error::{Error, Result};

/// How long a whole submission may take.
pub const SUBMIT_TIMEOUT: Duration = Duration::from_secs(60);

/// Sends `message` (raw RFC 5322 bytes) from `sender` to `recipient` through the
/// SMTP server at `addr` (`host:port`).
///
/// `sender` may be empty for a null reverse-path. Line endings in `message` are
/// normalised to CRLF and leading dots are escaped, so any message body is safe.
///
/// # Errors
/// Fails if an address contains characters that could alter the SMTP dialogue,
/// the server cannot be reached, answers unexpectedly or rejects the message, or
/// [`SUBMIT_TIMEOUT`] passes.
pub async fn submit(
    addr: &str,
    helo: &str,
    sender: &str,
    recipient: &str,
    message: &[u8],
) -> Result<()> {
    check_address(sender, true)?;
    check_address(recipient, false)?;
    check_helo(helo)?;
    timeout(
        SUBMIT_TIMEOUT,
        dialogue(addr, helo, sender, recipient, message),
    )
    .await
    .map_err(|_| Error::Provisioning("SMTP submission timed out".into()))?
}

fn check_address(address: &str, allow_empty: bool) -> Result<()> {
    if address.is_empty() && allow_empty {
        return Ok(());
    }
    let bad = address.is_empty()
        || address.len() > 320
        || address
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '<' | '>'));
    if bad {
        return Err(Error::Validation(
            "address contains characters not allowed in an SMTP command".into(),
        ));
    }
    Ok(())
}

fn check_helo(helo: &str) -> Result<()> {
    if helo.is_empty()
        || !helo
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        return Err(Error::Validation("invalid HELO name".into()));
    }
    Ok(())
}

fn smtp_error(what: &str, reply: &Reply) -> Error {
    Error::Provisioning(format!(
        "SMTP server refused {what}: {} {}",
        reply.code, reply.text
    ))
}

async fn dialogue(
    addr: &str,
    helo: &str,
    sender: &str,
    recipient: &str,
    message: &[u8],
) -> Result<()> {
    let stream = TcpStream::connect(addr)
        .await
        .map_err(|e| Error::Provisioning(format!("cannot reach the SMTP server: {e}")))?;
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);

    let greeting = reply(&mut read).await?;
    if greeting.code != 220 {
        return Err(smtp_error("the connection", &greeting));
    }
    command(&mut write, &format!("EHLO {helo}\r\n")).await?;
    let ehlo = reply(&mut read).await?;
    if ehlo.code != 250 {
        return Err(smtp_error("EHLO", &ehlo));
    }
    command(&mut write, &format!("MAIL FROM:<{sender}>\r\n")).await?;
    let mail = reply(&mut read).await?;
    if mail.code != 250 {
        return Err(smtp_error("the sender", &mail));
    }
    command(&mut write, &format!("RCPT TO:<{recipient}>\r\n")).await?;
    let rcpt = reply(&mut read).await?;
    if rcpt.code != 250 && rcpt.code != 251 {
        return Err(smtp_error("the recipient", &rcpt));
    }
    command(&mut write, "DATA\r\n").await?;
    let data = reply(&mut read).await?;
    if data.code != 354 {
        return Err(smtp_error("DATA", &data));
    }
    write
        .write_all(&dot_stuff(message))
        .await
        .map_err(io_error)?;
    write.write_all(b".\r\n").await.map_err(io_error)?;
    write.flush().await.map_err(io_error)?;
    let accepted = reply(&mut read).await?;
    if accepted.code != 250 {
        return Err(smtp_error("the message", &accepted));
    }
    // The message is queued; a failure to say goodbye does not matter.
    let _ = command(&mut write, "QUIT\r\n").await;
    Ok(())
}

fn io_error(error: std::io::Error) -> Error {
    Error::Provisioning(format!("SMTP connection error: {error}"))
}

/// Sends one command line (the caller supplies the trailing CRLF).
///
/// # Errors
/// Fails on I/O errors.
pub async fn command<W: AsyncWriteExt + Unpin>(write: &mut W, text: &str) -> Result<()> {
    write.write_all(text.as_bytes()).await.map_err(io_error)?;
    write.flush().await.map_err(io_error)
}

/// A (possibly multi-line) server reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    /// The three-digit status code.
    pub code: u16,
    /// The reply text, lines joined with spaces and shortened.
    pub text: String,
}

/// Longest reply line accepted, so a hostile server cannot make us buffer forever.
const MAX_REPLY_LINE: usize = 4096;

/// Reads one (possibly multi-line) reply from the server.
///
/// # Errors
/// Fails if the connection closes, the reply is malformed, or it is absurdly long.
pub async fn reply<R: AsyncBufReadExt + Unpin>(read: &mut R) -> Result<Reply> {
    let mut text = String::new();
    for _ in 0..100 {
        let mut line = String::new();
        let n = read.read_line(&mut line).await.map_err(io_error)?;
        if n == 0 {
            return Err(Error::Provisioning(
                "SMTP server closed the connection".into(),
            ));
        }
        if line.len() > MAX_REPLY_LINE {
            return Err(Error::Provisioning("SMTP reply line too long".into()));
        }
        let line = line.trim_end();
        let (code, rest) = line.split_at_checked(3).unwrap_or((line, ""));
        let Ok(code) = code.parse::<u16>() else {
            return Err(Error::Provisioning("malformed SMTP reply".into()));
        };
        if text.len() < 200 {
            text.push_str(rest.trim_start_matches(['-', ' ']));
            text.push(' ');
        }
        // "250-..." continues; "250 ..." (or a bare code) ends the reply.
        if !rest.starts_with('-') {
            return Ok(Reply {
                code,
                text: text.trim().to_owned(),
            });
        }
    }
    Err(Error::Provisioning("SMTP reply too long".into()))
}

/// Converts line endings to CRLF, escapes leading dots, and makes sure the data
/// ends with a line break (so the terminating `.` starts a line).
#[must_use]
pub fn dot_stuff(message: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(message.len() + 64);
    let mut at_line_start = true;
    let mut iter = message.iter().copied().peekable();
    while let Some(byte) = iter.next() {
        match byte {
            b'\r' if iter.peek() == Some(&b'\n') => {}
            b'\n' => {
                out.extend_from_slice(b"\r\n");
                at_line_start = true;
            }
            _ => {
                if at_line_start && byte == b'.' {
                    out.push(b'.');
                }
                out.push(byte);
                at_line_start = false;
            }
        }
    }
    if !at_line_start {
        out.extend_from_slice(b"\r\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    #[test]
    fn dot_stuffing_normalises_lines_and_escapes_dots() {
        assert_eq!(dot_stuff(b"a\nb"), b"a\r\nb\r\n");
        assert_eq!(dot_stuff(b"a\r\n.b\r\n..c\n"), b"a\r\n..b\r\n...c\r\n");
        assert_eq!(dot_stuff(b""), b"");
        assert_eq!(dot_stuff(b".\n"), b"..\r\n");
    }

    #[test]
    fn addresses_that_could_inject_commands_are_refused() {
        for bad in ["a@b\r\nRCPT TO:<c@d>", "a b@c", "<a@b>", "a@b>", ""] {
            assert!(check_address(bad, false).is_err(), "{bad:?}");
        }
        assert!(check_address("a@example.com", false).is_ok());
        assert!(check_address("", true).is_ok(), "null sender is allowed");
        assert!(check_helo("mail.example.com").is_ok());
        assert!(check_helo("bad host").is_err());
    }

    /// Serves one SMTP session, recording what the client sent.
    async fn fake_server(rcpt_reply: &'static str) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr").to_string();
        let handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (read, mut write) = stream.into_split();
            let mut read = BufReader::new(read);
            let mut seen = String::new();
            write.write_all(b"220 test ESMTP\r\n").await.expect("w");
            let mut line = String::new();
            loop {
                line.clear();
                if read.read_line(&mut line).await.expect("r") == 0 {
                    break;
                }
                seen.push_str(&line);
                let upper = line.to_ascii_uppercase();
                if upper.starts_with("EHLO") {
                    write
                        .write_all(b"250-test\r\n250 8BITMIME\r\n")
                        .await
                        .expect("w");
                } else if upper.starts_with("MAIL") {
                    write.write_all(b"250 ok\r\n").await.expect("w");
                } else if upper.starts_with("RCPT") {
                    write.write_all(rcpt_reply.as_bytes()).await.expect("w");
                } else if upper.starts_with("DATA") {
                    write.write_all(b"354 go\r\n").await.expect("w");
                    loop {
                        line.clear();
                        read.read_line(&mut line).await.expect("r");
                        seen.push_str(&line);
                        if line == ".\r\n" {
                            break;
                        }
                    }
                    write.write_all(b"250 queued\r\n").await.expect("w");
                } else if upper.starts_with("QUIT") {
                    write.write_all(b"221 bye\r\n").await.expect("w");
                    break;
                }
            }
            let mut rest = Vec::new();
            let _ = read.read_to_end(&mut rest).await;
            seen
        });
        (addr, handle)
    }

    #[tokio::test]
    async fn delivers_one_message_to_one_recipient() {
        let (addr, server) = fake_server("250 ok\r\n").await;
        submit(
            &addr,
            "rustmail.test",
            "s@example.org",
            "r@example.com",
            b"Subject: x\n\n.dot\nbody",
        )
        .await
        .expect("submit");
        let seen = server.await.expect("server");
        assert!(seen.contains("MAIL FROM:<s@example.org>\r\n"));
        assert!(seen.contains("RCPT TO:<r@example.com>\r\n"));
        assert!(
            seen.contains("\r\n..dot\r\n"),
            "leading dot escaped: {seen:?}"
        );
        assert!(seen.ends_with(".\r\n") || seen.contains("body\r\n.\r\n"));
    }

    #[tokio::test]
    async fn a_refused_recipient_is_an_error() {
        let (addr, _server) = fake_server("550 5.1.1 no such user\r\n").await;
        let error = submit(&addr, "rustmail.test", "", "r@example.com", b"x")
            .await
            .expect_err("must fail");
        assert!(error.to_string().contains("550"), "{error}");
    }

    #[tokio::test]
    async fn an_unreachable_server_is_an_error() {
        let error = submit("127.0.0.1:1", "rustmail.test", "", "r@example.com", b"x")
            .await
            .expect_err("must fail");
        assert!(error.to_string().contains("cannot reach"), "{error}");
    }
}
