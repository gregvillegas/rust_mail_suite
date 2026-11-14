//! Building outgoing messages.
//!
//! User input is never trusted to be a header value: addresses are parsed and
//! checked, line breaks are refused, and the MIME structure and header encoding
//! are produced by `mail-builder`, not by string concatenation.

use mail_builder::headers::address::Address as BuilderAddress;
use mail_builder::MessageBuilder;
use mail_parser::MessageParser;
use uuid::Uuid;

use crate::error::{MailError, MailResult};
use crate::message::Address;

/// Most recipients (To, Cc and Bcc together) on one message.
pub const MAX_RECIPIENTS: usize = 100;
/// Most attachments on one message.
pub const MAX_ATTACHMENTS: usize = 20;
/// Largest total size of attachments, before MIME encoding inflates it by about a third.
pub const MAX_ATTACHMENT_BYTES: usize = 25 * 1024 * 1024;
/// Longest subject.
pub const MAX_SUBJECT_CHARS: usize = 500;
/// Longest body, in bytes.
pub const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
/// Longest text accepted in one address field.
const MAX_ADDRESS_FIELD: usize = 4000;

/// A file to attach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutAttachment {
    /// File name shown to the recipient.
    pub filename: String,
    /// Declared content type (`application/octet-stream` if unknown).
    pub content_type: String,
    /// The bytes.
    pub data: Vec<u8>,
}

/// Everything needed to write a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    /// The sender. Must be the signed-in user's own address.
    pub from: Address,
    /// To.
    pub to: Vec<Address>,
    /// Cc.
    pub cc: Vec<Address>,
    /// Bcc: receives the message but is not written into it.
    pub bcc: Vec<Address>,
    /// Subject.
    pub subject: String,
    /// Plain text body.
    pub body: String,
    /// `Message-ID` of the message being answered.
    pub in_reply_to: Option<String>,
    /// The thread's `Message-ID`s, oldest first.
    pub references: Vec<String>,
    /// Attachments.
    pub attachments: Vec<OutAttachment>,
}

/// A message ready to hand to the mail server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltMessage {
    /// The complete RFC 5322 message.
    pub bytes: Vec<u8>,
    /// Its `Message-ID` (without angle brackets).
    pub message_id: String,
    /// Every envelope recipient: To, Cc and Bcc, without duplicates.
    pub recipients: Vec<String>,
}

/// Whether `email` is a plausible, safe address: one `@`, a sensible local part
/// and a domain with at least two dot-separated labels. Deliberately stricter
/// than RFC 5322: quoted local parts and bare domains are not accepted.
#[must_use]
pub fn is_valid_email(email: &str) -> bool {
    if email.len() > 254 {
        return false;
    }
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    if local.is_empty() || local.len() > 64 || domain.contains('@') {
        return false;
    }
    let local_ok = local
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+/=?^_`{|}~.-".contains(c))
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..");
    let labels: Vec<&str> = domain.split('.').collect();
    let domain_ok = labels.len() >= 2
        && labels.iter().all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        });
    local_ok && domain_ok
}

/// Parses what a person typed into an address field: `a@x.com, Bob <b@y.com>`.
///
/// # Errors
/// [`MailError::Invalid`] naming the problem, for control characters, overlong
/// input, or an address that is not valid.
pub fn parse_recipients(input: &str) -> MailResult<Vec<Address>> {
    if input.trim().is_empty() {
        return Ok(Vec::new());
    }
    if input.chars().any(|c| c.is_control()) {
        return Err(MailError::Invalid("an address contains a control character".into()));
    }
    if input.len() > MAX_ADDRESS_FIELD {
        return Err(MailError::Invalid("the address list is too long".into()));
    }
    let synthetic = format!("To: {input}\r\n\r\n");
    let parsed = MessageParser::new()
        .parse_headers(synthetic.as_bytes())
        .and_then(|m| {
            m.to().map(|list| {
                list.mailboxes()
                    .map(|mb| {
                        (
                            mb.name().map(str::to_owned).filter(|n| !n.trim().is_empty()),
                            mb.address().map(|a| a.trim().to_owned()),
                        )
                    })
                    .collect::<Vec<_>>()
            })
        })
        .unwrap_or_default();
    if parsed.is_empty() {
        return Err(MailError::Invalid(format!("\"{}\" is not an e-mail address", shorten(input))));
    }
    let mut out = Vec::with_capacity(parsed.len());
    for (name, email) in parsed {
        let email = email.unwrap_or_default();
        if !is_valid_email(&email) {
            return Err(MailError::Invalid(format!("\"{}\" is not a valid e-mail address", shorten(&email))));
        }
        out.push(Address { name, email });
    }
    Ok(out)
}

fn shorten(text: &str) -> String {
    let mut s: String = text.chars().take(60).collect();
    if text.chars().count() > 60 {
        s.push('…');
    }
    s
}

fn to_builder(a: &Address) -> BuilderAddress<'static> {
    match &a.name {
        Some(name) => BuilderAddress::from((name.clone(), a.email.clone())),
        None => BuilderAddress::from(a.email.clone()),
    }
}

/// Checks a draft and writes it as a message.
///
/// `helo_name` supplies the domain for a generated `Message-ID` if the sender's
/// own address has none usable.
///
/// # Errors
/// [`MailError::Invalid`] or [`MailError::TooLarge`] with a message fit for the user.
pub fn build(draft: &Draft, helo_name: &str) -> MailResult<BuiltMessage> {
    if !is_valid_email(&draft.from.email) {
        return Err(MailError::Invalid("the sender address is not valid".into()));
    }
    if draft.to.is_empty() && draft.cc.is_empty() && draft.bcc.is_empty() {
        return Err(MailError::Invalid("add at least one recipient".into()));
    }
    let mut recipients: Vec<String> = Vec::new();
    for a in draft.to.iter().chain(&draft.cc).chain(&draft.bcc) {
        if !is_valid_email(&a.email) {
            return Err(MailError::Invalid(format!("\"{}\" is not a valid e-mail address", shorten(&a.email))));
        }
        let lower = a.email.to_lowercase();
        if !recipients.contains(&lower) {
            recipients.push(lower);
        }
    }
    if recipients.len() > MAX_RECIPIENTS {
        return Err(MailError::Invalid(format!("at most {MAX_RECIPIENTS} recipients are allowed")));
    }
    if draft.subject.chars().any(|c| c == '\r' || c == '\n' || c == '\0') {
        return Err(MailError::Invalid("the subject must be a single line".into()));
    }
    if draft.subject.chars().count() > MAX_SUBJECT_CHARS {
        return Err(MailError::Invalid("the subject is too long".into()));
    }
    if draft.body.len() > MAX_BODY_BYTES {
        return Err(MailError::TooLarge("the message text is too long".into()));
    }
    if draft.attachments.len() > MAX_ATTACHMENTS {
        return Err(MailError::Invalid(format!("at most {MAX_ATTACHMENTS} attachments are allowed")));
    }
    let total: usize = draft.attachments.iter().map(|a| a.data.len()).sum();
    if total > MAX_ATTACHMENT_BYTES {
        return Err(MailError::TooLarge(format!(
            "the attachments are larger than {} MB in total",
            MAX_ATTACHMENT_BYTES / (1024 * 1024)
        )));
    }

    let domain = draft
        .from
        .email
        .rsplit_once('@')
        .map_or(helo_name, |(_, d)| d);
    let message_id = format!("{}@{domain}", Uuid::new_v4().simple());
    let mut builder = MessageBuilder::new()
        .message_id(message_id.clone())
        .from(to_builder(&draft.from))
        .subject(draft.subject.clone())
        .text_body(draft.body.clone())
        .date(chrono::Utc::now().timestamp());
    if !draft.to.is_empty() {
        builder = builder.to(draft.to.iter().map(to_builder).collect::<Vec<_>>());
    }
    if !draft.cc.is_empty() {
        builder = builder.cc(draft.cc.iter().map(to_builder).collect::<Vec<_>>());
    }
    if let Some(parent) = draft.in_reply_to.as_deref().map(clean_id).filter(|p| !p.is_empty()) {
        builder = builder.in_reply_to(parent);
    }
    let references: Vec<String> = draft
        .references
        .iter()
        .map(|r| clean_id(r))
        .filter(|r| !r.is_empty())
        .collect();
    if !references.is_empty() {
        builder = builder.references(references);
    }
    for a in &draft.attachments {
        builder = builder.attachment(
            a.content_type.clone(),
            crate::message::safe_filename(&a.filename),
            a.data.clone(),
        );
    }
    let bytes = builder
        .write_to_vec()
        .map_err(|e| MailError::Invalid(format!("the message could not be assembled: {e}")))?;
    Ok(BuiltMessage {
        bytes,
        message_id,
        recipients,
    })
}

/// A `Message-ID` value without angle brackets, or empty if it is unusable.
fn clean_id(id: &str) -> String {
    let id = id.trim().trim_start_matches('<').trim_end_matches('>');
    if id.is_empty() || id.len() > 300 || id.chars().any(|c| c.is_control() || c.is_whitespace() || c == '<' || c == '>') {
        String::new()
    } else {
        id.to_owned()
    }
}

/// `Re: subject`, without stacking prefixes.
#[must_use]
pub fn reply_subject(subject: &str) -> String {
    prefixed(subject, "Re:", &["re:", "aw:", "antw:"])
}

/// `Fwd: subject`, without stacking prefixes.
#[must_use]
pub fn forward_subject(subject: &str) -> String {
    prefixed(subject, "Fwd:", &["fwd:", "fw:", "wg:"])
}

fn prefixed(subject: &str, prefix: &str, existing: &[&str]) -> String {
    let trimmed = subject.trim();
    let lower = trimmed.to_lowercase();
    if existing.iter().any(|p| lower.starts_with(p)) {
        trimmed.to_owned()
    } else {
        format!("{prefix} {trimmed}").trim_end().to_owned()
    }
}

/// Quotes a message for a reply: `> ` before every line, under an attribution line.
#[must_use]
pub fn quote_for_reply(attribution: &str, text: &str) -> String {
    let quoted: String = text
        .replace("\r\n", "\n")
        .lines()
        .take(500)
        .map(|l| if l.is_empty() { ">\n".to_owned() } else { format!("> {l}\n") })
        .collect();
    format!("\n\n{attribution}\n{quoted}")
}

/// The body of a forwarded message: a header block then the original text.
#[must_use]
pub fn forward_body(from: &str, to: &str, date: &str, subject: &str, text: &str) -> String {
    format!(
        "\n\n---------- Forwarded message ----------\nFrom: {from}\nTo: {to}\nDate: {date}\nSubject: {subject}\n\n{}",
        text.replace("\r\n", "\n")
    )
}

/// The recipients of a reply-all: everyone but the user, without duplicates.
#[must_use]
pub fn reply_all_recipients(sender: &Address, to: &[Address], cc: &[Address], me: &str) -> (Vec<Address>, Vec<Address>) {
    let me = me.to_lowercase();
    let mut seen = vec![me.clone(), sender.email.to_lowercase()];
    let mut keep = |list: &[Address]| -> Vec<Address> {
        list.iter()
            .filter(|a| {
                let e = a.email.to_lowercase();
                if seen.contains(&e) {
                    false
                } else {
                    seen.push(e);
                    true
                }
            })
            .cloned()
            .collect()
    };
    let to_out = keep(to);
    let cc_out = keep(cc);
    (to_out, cc_out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::parse;

    fn addr(name: Option<&str>, email: &str) -> Address {
        Address { name: name.map(str::to_owned), email: email.to_owned() }
    }

    fn draft() -> Draft {
        Draft {
            from: addr(Some("Alice"), "alice@example.com"),
            to: vec![addr(Some("Bob"), "bob@example.org")],
            cc: vec![],
            bcc: vec![],
            subject: "Hello".into(),
            body: "Hi Bob".into(),
            in_reply_to: None,
            references: vec![],
            attachments: vec![],
        }
    }

    #[test]
    fn email_validation() {
        for ok in ["a@b.co", "first.last@sub.example.com", "o'brien+tag@example.org", "x_y@a-b.example"] {
            assert!(is_valid_email(ok), "{ok}");
        }
        for bad in [
            "", "a", "a@", "@b.co", "a@b", "a@@b.co", "a b@c.de", "a@b..co", "a@-b.co", ".a@b.co", "a.@b.co",
            "a..b@c.de", "a@b.c\r\nBcc: x@y.z", "<a@b.co>", "a@b.co>", "\"a b\"@c.de",
        ] {
            assert!(!is_valid_email(bad), "{bad:?}");
        }
        assert!(!is_valid_email(&format!("{}@b.co", "x".repeat(65))));
    }

    #[test]
    fn recipient_lists_are_parsed() {
        let list = parse_recipients("Bob <bob@example.org>, carol@example.org").unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0], addr(Some("Bob"), "bob@example.org"));
        assert_eq!(parse_recipients("   ").unwrap(), vec![]);
    }

    #[test]
    fn address_fields_cannot_inject_headers() {
        for bad in [
            "a@b.co\r\nBcc: victim@example.com",
            "a@b.co\nBcc: v@e.com",
            "a@b.co\0",
            "not an address",
            "a@b",
            "<>",
        ] {
            assert!(parse_recipients(bad).is_err(), "{bad:?}");
        }
        assert!(parse_recipients(&"a@b.co,".repeat(1000)).is_err(), "overlong lists are refused");
    }

    #[test]
    fn a_message_round_trips_through_the_parser() {
        let mut d = draft();
        d.cc = vec![addr(None, "carol@example.org")];
        d.bcc = vec![addr(None, "secret@example.net")];
        d.subject = "Grüße & more".into();
        d.body = "Line one\nLine two — with a dash\n".into();
        let built = build(&d, "rustmail.local").unwrap();
        assert_eq!(built.recipients, ["bob@example.org", "carol@example.org", "secret@example.net"]);
        let text = String::from_utf8_lossy(&built.bytes).into_owned();
        assert!(!text.to_lowercase().contains("secret@example.net"), "Bcc must not appear in the message");
        assert!(!text.to_lowercase().contains("\nbcc:"));
        let back = parse(&built.bytes).unwrap();
        assert_eq!(back.subject, "Grüße & more");
        assert_eq!(back.from.unwrap().email, "alice@example.com");
        assert_eq!(back.to[0].email, "bob@example.org");
        assert!(back.text.contains("Line two — with a dash"));
        assert_eq!(back.message_id.as_deref(), Some(built.message_id.as_str()));
        assert!(built.message_id.ends_with("@example.com"));
    }

    #[test]
    fn replies_carry_the_thread_headers() {
        let mut d = draft();
        d.in_reply_to = Some("<parent@x>".into());
        d.references = vec!["<root@x>".into(), "parent@x".into()];
        let built = build(&d, "h.local").unwrap();
        let back = parse(&built.bytes).unwrap();
        assert_eq!(back.in_reply_to.as_deref(), Some("parent@x"));
        assert_eq!(back.references, ["root@x", "parent@x"]);
    }

    #[test]
    fn attachments_are_included_with_safe_names() {
        let mut d = draft();
        d.attachments = vec![OutAttachment {
            filename: "../../report.pdf".into(),
            content_type: "application/pdf".into(),
            data: b"%PDF-1.4 test".to_vec(),
        }];
        let built = build(&d, "h.local").unwrap();
        let back = parse(&built.bytes).unwrap();
        assert_eq!(back.attachments.len(), 1);
        assert_eq!(back.attachments[0].filename, "report.pdf");
        assert_eq!(crate::message::attachment(&built.bytes, 0).unwrap().data, b"%PDF-1.4 test");
    }

    #[test]
    fn limits_are_enforced() {
        let mut d = draft();
        d.to.clear();
        assert!(build(&d, "h").is_err(), "no recipients");

        let mut d = draft();
        d.subject = "a\r\nBcc: x@y.z".into();
        assert!(build(&d, "h").is_err(), "line break in subject");

        let mut d = draft();
        d.to = (0..=MAX_RECIPIENTS).map(|i| addr(None, &format!("u{i}@example.org"))).collect();
        assert!(build(&d, "h").is_err(), "too many recipients");

        let mut d = draft();
        d.attachments = vec![OutAttachment {
            filename: "big.bin".into(),
            content_type: "application/octet-stream".into(),
            data: vec![0; MAX_ATTACHMENT_BYTES + 1],
        }];
        assert!(matches!(build(&d, "h"), Err(MailError::TooLarge(_))));

        let mut d = draft();
        d.from = addr(None, "not-an-address");
        assert!(build(&d, "h").is_err(), "bad sender");

        let mut d = draft();
        d.to = vec![addr(None, "bad@@x")];
        assert!(build(&d, "h").is_err(), "bad recipient");
    }

    #[test]
    fn duplicate_recipients_are_sent_once() {
        let mut d = draft();
        d.cc = vec![addr(None, "BOB@example.org")];
        assert_eq!(build(&d, "h.local").unwrap().recipients, ["bob@example.org"]);
    }

    #[test]
    fn subjects_do_not_stack_prefixes() {
        assert_eq!(reply_subject("Hello"), "Re: Hello");
        assert_eq!(reply_subject("Re: Hello"), "Re: Hello");
        assert_eq!(reply_subject("AW: Hallo"), "AW: Hallo");
        assert_eq!(reply_subject(""), "Re:");
        assert_eq!(forward_subject("Hello"), "Fwd: Hello");
        assert_eq!(forward_subject("FW: Hello"), "FW: Hello");
    }

    #[test]
    fn quoting_prefixes_every_line() {
        let q = quote_for_reply("On Monday, Bob wrote:", "one\n\ntwo\r\nthree");
        assert_eq!(q, "\n\nOn Monday, Bob wrote:\n> one\n>\n> two\n> three\n");
        let long = "x\n".repeat(2000);
        assert_eq!(quote_for_reply("a", &long).lines().filter(|l| l.starts_with("> ")).count(), 500);
    }

    #[test]
    fn reply_all_excludes_me_and_duplicates() {
        let sender = addr(None, "bob@example.org");
        let to = vec![addr(None, "alice@example.com"), addr(None, "carol@example.org")];
        let cc = vec![addr(None, "CAROL@example.org"), addr(None, "dan@example.org"), addr(None, "bob@example.org")];
        let (to_out, cc_out) = reply_all_recipients(&sender, &to, &cc, "Alice@Example.com");
        assert_eq!(to_out.iter().map(|a| a.email.as_str()).collect::<Vec<_>>(), ["carol@example.org"]);
        assert_eq!(cc_out.iter().map(|a| a.email.as_str()).collect::<Vec<_>>(), ["dan@example.org"]);
    }
}
