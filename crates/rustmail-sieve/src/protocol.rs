//! The ManageSieve wire format (RFC 5804): tokens and responses.
//!
//! A server line is a sequence of quoted strings, atoms, parentheses and (at the end
//! of a line) a literal announcement `{N}` or `{N+}`, which is followed by `N` bytes of
//! data and then the rest of the line.

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt};

use crate::error::{SieveError, SieveResult};

/// The longest line or literal accepted from a server. Scripts are limited to 64 KB
/// by RustMail, so this leaves ample room while bounding memory.
pub const MAX_ITEM_BYTES: usize = 1024 * 1024;

/// One piece of a response line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    /// A quoted string or literal.
    Str(String),
    /// A bare word such as `OK`, `NO`, `ACTIVE`.
    Atom(String),
    /// `(`
    Open,
    /// `)`
    Close,
}

/// How a response ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The command worked.
    Ok,
    /// The server refused it.
    No,
    /// The server is closing the connection.
    Bye,
}

/// A complete response: the data lines, then the status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// Lines before the status line.
    pub lines: Vec<Vec<Token>>,
    /// How it ended.
    pub status: Status,
    /// The human-readable text of the status line, if any (for `NO`, the reason).
    pub text: String,
}

/// What the tokenizer found at the end of a line.
#[derive(Debug, PartialEq, Eq)]
pub struct Tokenized {
    /// Tokens on the line.
    pub tokens: Vec<Token>,
    /// A literal of this many bytes follows the line.
    pub literal: Option<usize>,
}

/// Splits one line (without its CRLF) into tokens.
///
/// # Errors
/// [`SieveError::Protocol`] for an unterminated string or a malformed literal.
pub fn tokenize(line: &str) -> SieveResult<Tokenized> {
    let bad = |what: &str| SieveError::Protocol(format!("{what} in server line"));
    let mut tokens = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' => {
                chars.next();
            }
            '(' => {
                chars.next();
                tokens.push(Token::Open);
            }
            ')' => {
                chars.next();
                tokens.push(Token::Close);
            }
            '"' => {
                chars.next();
                let mut text = String::new();
                loop {
                    match chars.next() {
                        Some('\\') => {
                            text.push(chars.next().ok_or_else(|| bad("unterminated string"))?)
                        }
                        Some('"') => break,
                        Some(ch) => text.push(ch),
                        None => return Err(bad("unterminated string")),
                    }
                }
                tokens.push(Token::Str(text));
            }
            '{' => {
                chars.next();
                let digits: String = chars.by_ref().take_while(|c| *c != '}').collect();
                let digits = digits.strip_suffix('+').unwrap_or(&digits);
                let n: usize = digits.parse().map_err(|_| bad("bad literal size"))?;
                if chars.peek().is_some() {
                    return Err(bad("text after a literal size"));
                }
                return Ok(Tokenized {
                    tokens,
                    literal: Some(n),
                });
            }
            _ => {
                let mut word = String::new();
                while let Some(&ch) = chars.peek() {
                    if matches!(ch, ' ' | '\t' | '(' | ')' | '"') {
                        break;
                    }
                    word.push(ch);
                    chars.next();
                }
                tokens.push(Token::Atom(word));
            }
        }
    }
    Ok(Tokenized {
        tokens,
        literal: None,
    })
}

async fn read_line<R: AsyncBufRead + Unpin>(reader: &mut R) -> SieveResult<String> {
    let mut raw = Vec::new();
    let n = reader
        .take(MAX_ITEM_BYTES as u64)
        .read_until(b'\n', &mut raw)
        .await?;
    if n == 0 {
        return Err(SieveError::Protocol(
            "the server closed the connection".into(),
        ));
    }
    if raw.last() != Some(&b'\n') {
        return Err(SieveError::Protocol("a server line is too long".into()));
    }
    let text = String::from_utf8(raw)
        .map_err(|_| SieveError::Protocol("the server sent text that is not UTF-8".into()))?;
    Ok(text.trim_end_matches(['\r', '\n']).to_owned())
}

/// Reads one line, following literals, and returns its tokens.
async fn read_tokens<R: AsyncBufRead + Unpin>(reader: &mut R) -> SieveResult<Vec<Token>> {
    let mut all = Vec::new();
    loop {
        let Tokenized { tokens, literal } = tokenize(&read_line(reader).await?)?;
        all.extend(tokens);
        let Some(n) = literal else { return Ok(all) };
        if n > MAX_ITEM_BYTES {
            return Err(SieveError::Protocol(
                "the server announced too much data".into(),
            ));
        }
        let mut data = vec![0_u8; n];
        reader.read_exact(&mut data).await?;
        all.push(Token::Str(String::from_utf8(data).map_err(|_| {
            SieveError::Protocol("the server sent text that is not UTF-8".into())
        })?));
    }
}

/// Reads a complete response.
///
/// # Errors
/// [`SieveError::Protocol`] for malformed input or a closed connection, or an I/O error.
pub async fn read_response<R: AsyncBufRead + Unpin>(reader: &mut R) -> SieveResult<Response> {
    let mut lines = Vec::new();
    loop {
        let tokens = read_tokens(reader).await?;
        let status = match tokens.first() {
            Some(Token::Atom(a)) if a.eq_ignore_ascii_case("OK") => Some(Status::Ok),
            Some(Token::Atom(a)) if a.eq_ignore_ascii_case("NO") => Some(Status::No),
            Some(Token::Atom(a)) if a.eq_ignore_ascii_case("BYE") => Some(Status::Bye),
            _ => None,
        };
        match status {
            Some(status) => {
                // The text is the last string; a "(CODE ...)" before it is ignored.
                let text = tokens
                    .iter()
                    .rev()
                    .find_map(|t| {
                        if let Token::Str(s) = t {
                            Some(s.clone())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default();
                return Ok(Response {
                    lines,
                    status,
                    text,
                });
            }
            None => lines.push(tokens),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(t: &str) -> Token {
        Token::Str(t.into())
    }

    fn a(t: &str) -> Token {
        Token::Atom(t.into())
    }

    #[test]
    fn tokenizes_strings_atoms_and_codes() {
        let t = tokenize(r#""IMPLEMENTATION" "Dovecot Pigeonhole""#).unwrap();
        assert_eq!(t.tokens, vec![s("IMPLEMENTATION"), s("Dovecot Pigeonhole")]);
        let t = tokenize(r#""vacation" ACTIVE"#).unwrap();
        assert_eq!(t.tokens, vec![s("vacation"), a("ACTIVE")]);
        let t = tokenize(r#"NO (QUOTA/MAXSCRIPTS) "too many""#).unwrap();
        assert_eq!(
            t.tokens,
            vec![
                a("NO"),
                Token::Open,
                a("QUOTA/MAXSCRIPTS"),
                Token::Close,
                s("too many")
            ]
        );
        let t = tokenize(r#""a \"quoted\" \\ name""#).unwrap();
        assert_eq!(t.tokens, vec![s(r#"a "quoted" \ name"#)]);
    }

    #[test]
    fn literals_are_announced() {
        assert_eq!(
            tokenize("{12}").unwrap(),
            Tokenized {
                tokens: vec![],
                literal: Some(12)
            }
        );
        assert_eq!(tokenize("{7+}").unwrap().literal, Some(7));
        assert_eq!(tokenize("\"x\" {3}").unwrap().tokens, vec![s("x")]);
        for bad in ["\"open", "{abc}", "{5} trailing", "\"x\\"] {
            assert!(tokenize(bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn reads_capabilities() {
        let wire = b"\"IMPLEMENTATION\" \"Dovecot Pigeonhole\"\r\n\"SIEVE\" \"fileinto vacation\"\r\n\"STARTTLS\"\r\nOK \"Dovecot ready.\"\r\n";
        let r = read_response(&mut &wire[..]).await.unwrap();
        assert_eq!(
            (r.status, r.lines.len(), r.text.as_str()),
            (Status::Ok, 3, "Dovecot ready.")
        );
        assert_eq!(r.lines[2], vec![s("STARTTLS")]);
    }

    #[tokio::test]
    async fn reads_a_script_literal_with_awkward_content() {
        // The script contains quotes, a line that looks like a status, and CRLF.
        let body = "if true {\r\n  \"OK\"\r\nNO \"fake\"\r\n}\r\n";
        let wire = format!(
            "{{{}}}\r\n{body}\r\nOK \"Getscript completed.\"\r\n",
            body.len()
        );
        let r = read_response(&mut wire.as_bytes()).await.unwrap();
        assert_eq!(r.status, Status::Ok);
        assert_eq!(
            r.lines,
            vec![vec![s(body)]],
            "the literal is data, whatever it contains"
        );
    }

    #[tokio::test]
    async fn refusals_carry_their_reason() {
        let wire = b"NO \"line 3: error: unknown command 'fileintoo'.\"\r\n";
        let r = read_response(&mut &wire[..]).await.unwrap();
        assert_eq!(r.status, Status::No);
        assert!(r.text.contains("unknown command"));
        let r =
            read_response(&mut &b"NO (NONEXISTENT) \"There is no script by that name\"\r\n"[..])
                .await
                .unwrap();
        assert_eq!(r.text, "There is no script by that name");
    }

    #[tokio::test]
    async fn hostile_servers_cannot_exhaust_memory_or_hang_the_parser() {
        let wire = format!("{{{}}}\r\n", MAX_ITEM_BYTES + 1);
        assert!(read_response(&mut wire.as_bytes()).await.is_err());
        let long = vec![b'a'; MAX_ITEM_BYTES + 10];
        assert!(read_response(&mut &long[..]).await.is_err());
        assert!(
            read_response(&mut &b""[..]).await.is_err(),
            "a closed connection is an error"
        );
        assert!(
            read_response(&mut &b"{5}\r\nab"[..]).await.is_err(),
            "a short literal is an error"
        );
    }
}
