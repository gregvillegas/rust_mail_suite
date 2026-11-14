//! Reading messages: list summaries, full parsing, attachments, and HTML
//! sanitising.
//!
//! Mail is untrusted input. Everything here assumes hostile content: the parser
//! is tolerant, display text is returned as plain strings for the caller to
//! escape, and HTML only leaves this module after [`sanitize_html`].

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use mail_parser::{html_to_text, preview_text, MessageParser};
use serde::Serialize;

use crate::error::{MailError, MailResult};

/// What is fetched for each line of the message list: the header block only.
pub const SUMMARY_HEADERS: &str = "BODY.PEEK[HEADER]";
/// Messages larger than this are not fetched for display.
pub const MAX_MESSAGE_BYTES: u32 = 25 * 1024 * 1024;
/// How much text to keep as a search snippet.
pub const SNIPPET_CHARS: usize = 240;

/// A name and an e-mail address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Address {
    /// Display name, if any.
    pub name: Option<String>,
    /// The address.
    pub email: String,
}

impl Address {
    /// `Name <address>` or just the address, for display.
    #[must_use]
    pub fn display(&self) -> String {
        match self
            .name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
        {
            Some(name) if name != self.email => format!("{name} <{}>", self.email),
            _ => self.email.clone(),
        }
    }

    /// The name if there is one, else the address: for narrow columns.
    #[must_use]
    pub fn short(&self) -> String {
        self.name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or(&self.email)
            .to_owned()
    }
}

/// The IMAP flags the webmail cares about.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Flags {
    /// `\Seen`
    pub seen: bool,
    /// `\Answered`
    pub answered: bool,
    /// `\Flagged` (starred)
    pub flagged: bool,
    /// `\Draft`
    pub draft: bool,
    /// `\Deleted`
    pub deleted: bool,
}

impl Flags {
    /// Builds the set from flag names such as `\Seen`.
    pub fn from_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        let mut flags = Self::default();
        for name in names {
            match name.to_ascii_lowercase().as_str() {
                "\\seen" => flags.seen = true,
                "\\answered" => flags.answered = true,
                "\\flagged" => flags.flagged = true,
                "\\draft" => flags.draft = true,
                "\\deleted" => flags.deleted = true,
                _ => {}
            }
        }
        flags
    }

    /// The flags as IMAP names, for storing in the index.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        [
            (self.seen, "\\Seen"),
            (self.answered, "\\Answered"),
            (self.flagged, "\\Flagged"),
            (self.draft, "\\Draft"),
            (self.deleted, "\\Deleted"),
        ]
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, n)| n.to_owned())
        .collect()
    }
}

/// One line in the message list.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MessageSummary {
    /// IMAP UID within the folder.
    pub uid: u32,
    /// Flags.
    pub flags: Flags,
    /// Sender.
    pub from: Option<Address>,
    /// Recipients.
    pub to: Vec<Address>,
    /// Cc recipients.
    pub cc: Vec<Address>,
    /// Subject ("" if none).
    pub subject: String,
    /// The `Date` header.
    pub date: Option<DateTime<Utc>>,
    /// When the server received it (falls back to `date`).
    pub received: Option<DateTime<Utc>>,
    /// Size in bytes.
    pub size: u32,
    /// Whether it looks like it has attachments (multipart/mixed).
    pub has_attachments: bool,
    /// `Message-ID` header.
    pub message_id: Option<String>,
    /// `In-Reply-To` header.
    pub in_reply_to: Option<String>,
}

fn to_utc(date: &mail_parser::DateTime) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp(date.to_timestamp(), 0)
}

fn addresses(list: Option<mail_parser::AddressList<'_>>) -> Vec<Address> {
    list.map(|l| {
        l.mailboxes()
            .filter_map(|m| {
                let email = m.address()?.trim();
                (!email.is_empty()).then(|| Address {
                    name: m.name().map(str::to_owned).filter(|n| !n.trim().is_empty()),
                    email: email.to_owned(),
                })
            })
            .collect()
    })
    .unwrap_or_default()
}

/// Builds a summary from the header block fetched with [`SUMMARY_HEADERS`].
#[must_use]
pub fn summary_from_headers(
    uid: u32,
    flags: Flags,
    size: u32,
    internal_date: Option<DateTime<Utc>>,
    header_bytes: &[u8],
) -> MessageSummary {
    let parsed = MessageParser::new().parse_headers(header_bytes);
    let Some(m) = parsed else {
        return MessageSummary {
            uid,
            flags,
            from: None,
            to: Vec::new(),
            cc: Vec::new(),
            subject: String::new(),
            date: None,
            received: internal_date,
            size,
            has_attachments: false,
            message_id: None,
            in_reply_to: None,
        };
    };
    let date = m.date().and_then(|d| to_utc(&d));
    let has_attachments = m.content_type().is_some_and(|ct| {
        ct.ctype().eq_ignore_ascii_case("multipart")
            && ct
                .subtype()
                .is_some_and(|s| s.eq_ignore_ascii_case("mixed"))
    });
    MessageSummary {
        uid,
        flags,
        from: addresses(m.from()).into_iter().next(),
        to: addresses(m.to()),
        cc: addresses(m.cc()),
        subject: m.subject().unwrap_or("").trim().to_owned(),
        date,
        received: internal_date.or(date),
        size,
        has_attachments,
        message_id: m.message_id().map(str::to_owned),
        in_reply_to: m.in_reply_to().and_then(|l| l.first()).map(str::to_owned),
    }
}

/// A file attached to a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Attachment {
    /// Position among the message's attachments, used to download it.
    pub index: usize,
    /// A safe file name.
    pub filename: String,
    /// The declared content type.
    pub content_type: String,
    /// Decoded size in bytes.
    pub size: usize,
}

/// A fully parsed message, ready for display.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedMessage {
    /// Sender.
    pub from: Option<Address>,
    /// To.
    pub to: Vec<Address>,
    /// Cc.
    pub cc: Vec<Address>,
    /// Reply-To.
    pub reply_to: Vec<Address>,
    /// Subject.
    pub subject: String,
    /// `Date` header.
    pub date: Option<DateTime<Utc>>,
    /// `Message-ID`.
    pub message_id: Option<String>,
    /// `References`, oldest first.
    pub references: Vec<String>,
    /// `In-Reply-To`.
    pub in_reply_to: Option<String>,
    /// Plain text body (converted from HTML if that is all there is).
    pub text: String,
    /// HTML body, **unsanitised**; pass through [`sanitize_html`] before display.
    pub html: Option<String>,
    /// Attachments.
    pub attachments: Vec<Attachment>,
}

/// Parses a raw message.
///
/// # Errors
/// [`MailError::Parse`] if there is no recognisable message.
pub fn parse(raw: &[u8]) -> MailResult<ParsedMessage> {
    let m = MessageParser::new()
        .parse(raw)
        .ok_or_else(|| MailError::Parse("not a valid e-mail message".into()))?;
    // The parser's body lists follow RFC 8621: a plain-text message also appears as an
    // "HTML body" (converted). Only genuine text/html parts count as HTML here.
    let html: Vec<String> = m
        .html_body()
        .filter(|p| p.is_content_type("text", "html"))
        .filter_map(|p| p.text().map(std::borrow::Cow::into_owned))
        .collect();
    let html = (!html.is_empty()).then(|| html.join("\n"));
    let texts: Vec<String> = m
        .text_body()
        // A part without a Content-Type header is plain text by default.
        .filter(|p| !p.is_content_type("text", "html"))
        .filter_map(|p| p.text().map(std::borrow::Cow::into_owned))
        .collect();
    let text = if texts.is_empty() {
        html.as_deref().map(html_to_text).unwrap_or_default()
    } else {
        texts.join("\n")
    };
    let attachments = m
        .attachments()
        .enumerate()
        .map(|(index, part)| Attachment {
            index,
            filename: attachment_filename(part.attachment_name(), part.is_message(), index),
            content_type: part
                .content_type()
                .map(|ct| match ct.subtype() {
                    Some(sub) => format!("{}/{}", ct.ctype(), sub),
                    None => ct.ctype().to_owned(),
                })
                .unwrap_or_else(|| "application/octet-stream".to_owned()),
            size: part.decoded_len(),
        })
        .collect();
    Ok(ParsedMessage {
        from: addresses(m.from()).into_iter().next(),
        to: addresses(m.to()),
        cc: addresses(m.cc()),
        reply_to: addresses(m.reply_to()),
        subject: m.subject().unwrap_or("").trim().to_owned(),
        date: m.date().and_then(|d| to_utc(&d)),
        message_id: m.message_id().map(str::to_owned),
        references: m
            .references()
            .map(|l| {
                (0..l.len())
                    .filter_map(|i| l.get(i))
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        in_reply_to: m.in_reply_to().and_then(|l| l.first()).map(str::to_owned),
        text,
        html,
        attachments,
    })
}

/// A short plain-text excerpt for the search index.
#[must_use]
pub fn snippet(parsed: &ParsedMessage) -> String {
    let collapsed = parsed.text.split_whitespace().collect::<Vec<_>>().join(" ");
    preview_text(&collapsed, SNIPPET_CHARS).into_owned()
}

/// Makes a name safe to offer for download: no path parts, no control
/// characters, bounded length.
#[must_use]
pub fn safe_filename(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = last
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '"' | ':' | '*' | '?' | '<' | '>' | '|'))
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim();
    let cleaned: String = cleaned.chars().take(150).collect();
    if cleaned.is_empty() {
        "attachment".to_owned()
    } else {
        cleaned
    }
}

fn attachment_filename(name: Option<&str>, is_message: bool, index: usize) -> String {
    match name {
        Some(n) if !n.trim().is_empty() => safe_filename(n),
        _ if is_message => format!("message-{}.eml", index + 1),
        _ => format!("attachment-{}", index + 1),
    }
}

/// An attachment's bytes, ready to send to the browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentData {
    /// A safe file name.
    pub filename: String,
    /// The declared content type.
    pub content_type: String,
    /// Decoded content.
    pub data: Vec<u8>,
}

/// Extracts one attachment from a raw message.
///
/// # Errors
/// [`MailError::Parse`] if the message is unreadable, [`MailError::NotFound`] if
/// there is no attachment with that index.
pub fn attachment(raw: &[u8], index: usize) -> MailResult<AttachmentData> {
    let m = MessageParser::new()
        .parse(raw)
        .ok_or_else(|| MailError::Parse("not a valid e-mail message".into()))?;
    let part = m.attachments().nth(index).ok_or(MailError::NotFound)?;
    let content_type = part
        .content_type()
        .map(|ct| match ct.subtype() {
            Some(sub) => format!("{}/{}", ct.ctype(), sub),
            None => ct.ctype().to_owned(),
        })
        .unwrap_or_else(|| "application/octet-stream".to_owned());
    Ok(AttachmentData {
        filename: attachment_filename(part.attachment_name(), part.is_message(), index),
        content_type,
        data: if part.is_message() {
            part.raw().to_vec()
        } else {
            part.decoded().into_owned()
        },
    })
}

fn sanitizer(allow_remote_images: bool) -> ammonia::Builder<'static> {
    let mut builder = ammonia::Builder::default();
    builder
        .url_relative(ammonia::UrlRelative::Deny)
        .url_schemes(HashSet::from(["http", "https", "mailto"]))
        .link_rel(Some("noopener noreferrer nofollow"))
        .set_tag_attribute_value("a", "target", "_blank")
        .attribute_filter(move |element, attribute, value| {
            if element == "img" && attribute == "src" {
                // Remote images are how senders learn that (and when, and from
                // where) a message was opened, so they are off unless asked for.
                let remote = value.starts_with("http://") || value.starts_with("https://");
                return (allow_remote_images && remote).then(|| value.to_owned().into());
            }
            Some(value.into())
        });
    builder
}

/// Cleans HTML mail for display: scripts, styles, forms, frames, event handlers
/// and `javascript:` links are removed; links open in a new window without a
/// referrer; images are removed unless `allow_remote_images` is set.
#[must_use]
pub fn sanitize_html(html: &str, allow_remote_images: bool) -> String {
    sanitizer(allow_remote_images).clean(html).to_string()
}

/// Whether the HTML refers to images that [`sanitize_html`] would remove.
#[must_use]
pub fn has_remote_images(html: &str) -> bool {
    sanitize_html(html, true) != sanitize_html(html, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIMPLE: &[u8] = b"From: \"Ann Lee\" <ann@example.com>\r\nTo: Bob <bob@example.com>, carol@example.com\r\nCc: dan@example.com\r\nSubject: =?UTF-8?Q?Gr=C3=BC=C3=9Fe?=\r\nDate: Tue, 11 Nov 2025 09:30:00 +0100\r\nMessage-ID: <abc@example.com>\r\nReferences: <r1@x> <r2@x>\r\nIn-Reply-To: <r2@x>\r\n\r\nHello Bob,\r\nsee you soon.\r\n";

    #[test]
    fn headers_are_decoded_for_the_list() {
        let s = summary_from_headers(7, Flags::default(), 1234, None, SIMPLE);
        assert_eq!(s.uid, 7);
        assert_eq!(s.subject, "Grüße");
        assert_eq!(
            s.from.as_ref().map(Address::display).as_deref(),
            Some("Ann Lee <ann@example.com>")
        );
        assert_eq!(s.to.len(), 2);
        assert_eq!(s.cc[0].email, "dan@example.com");
        assert_eq!(
            s.date.map(|d| d.to_rfc3339()).as_deref(),
            Some("2025-11-11T08:30:00+00:00")
        );
        assert_eq!(s.message_id.as_deref(), Some("abc@example.com"));
        assert!(!s.has_attachments);
    }

    #[test]
    fn garbage_headers_do_not_panic() {
        for junk in [
            &b""[..],
            b"\x00\xff\xfe",
            b"no colon here",
            b"Subject:\r\n\r\n",
            b"From: <<<>>>\r\n\r\n",
        ] {
            let s = summary_from_headers(1, Flags::default(), 0, None, junk);
            assert_eq!(s.uid, 1);
        }
    }

    #[test]
    fn multipart_mixed_looks_like_it_has_attachments() {
        let h = b"Subject: x\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n";
        assert!(summary_from_headers(1, Flags::default(), 0, None, h).has_attachments);
    }

    #[test]
    fn a_plain_message_is_parsed_in_full() {
        let m = parse(SIMPLE).unwrap();
        assert_eq!(m.subject, "Grüße");
        assert_eq!(m.text.trim(), "Hello Bob,\r\nsee you soon.");
        assert!(m.html.is_none());
        assert_eq!(m.references, ["r1@x", "r2@x"]);
        assert_eq!(m.in_reply_to.as_deref(), Some("r2@x"));
        assert!(m.attachments.is_empty());
        assert_eq!(snippet(&m), "Hello Bob, see you soon.");
    }

    const WITH_ATTACHMENT: &[u8] = b"From: a@example.com\r\nTo: b@example.com\r\nSubject: files\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"B\"\r\n\r\n--B\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nsee attached\r\n--B\r\nContent-Type: application/pdf; name=\"../../etc/passwd.pdf\"\r\nContent-Disposition: attachment; filename=\"../../etc/passwd.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\nSGVsbG8gUERG\r\n--B--\r\n";

    #[test]
    fn attachments_are_listed_and_extracted_with_safe_names() {
        let m = parse(WITH_ATTACHMENT).unwrap();
        assert_eq!(m.attachments.len(), 1);
        let a = &m.attachments[0];
        assert_eq!(a.filename, "passwd.pdf", "no path components survive");
        assert_eq!(a.content_type, "application/pdf");
        assert_eq!(a.size, 9);
        let data = attachment(WITH_ATTACHMENT, 0).unwrap();
        assert_eq!(data.data, b"Hello PDF");
        assert!(matches!(
            attachment(WITH_ATTACHMENT, 1),
            Err(MailError::NotFound)
        ));
    }

    #[test]
    fn html_only_messages_get_a_text_version() {
        let raw = b"From: a@b.c\r\nTo: d@e.f\r\nSubject: s\r\nContent-Type: text/html\r\n\r\n<p>Hello <b>world</b></p>";
        let m = parse(raw).unwrap();
        assert!(m.html.is_some());
        assert!(m.text.contains("Hello") && m.text.contains("world") && !m.text.contains('<'));
    }

    #[test]
    fn file_names_are_made_safe() {
        for (input, expected) in [
            ("report.pdf", "report.pdf"),
            ("../../etc/passwd", "passwd"),
            ("C:\\Windows\\evil.exe", "evil.exe"),
            ("a\r\nb.txt", "ab.txt"),
            ("..", "attachment"),
            ("", "attachment"),
            (".hidden", "hidden"),
            ("na\"me?.txt", "name.txt"),
        ] {
            assert_eq!(safe_filename(input), expected, "{input:?}");
        }
        assert_eq!(safe_filename(&"x".repeat(500)).chars().count(), 150);
    }

    // ── sanitising hostile HTML ─────────────────────────────────────────────

    fn assert_clean(html: &str, remote_images: bool) -> String {
        let out = sanitize_html(html, remote_images).to_lowercase();
        for bad in [
            "<script",
            "javascript:",
            "onerror",
            "onclick",
            "onload",
            "<iframe",
            "<object",
            "<embed",
            "<form",
            "<style",
            "<link",
            "<meta",
            "<base",
            "expression(",
        ] {
            assert!(!out.contains(bad), "{bad} survived in {out}");
        }
        out
    }

    #[test]
    fn scripts_handlers_and_dangerous_urls_are_removed() {
        for html in [
            "<script>alert(1)</script><p>hi</p>",
            "<img src=x onerror=alert(1)>",
            "<a href=\"javascript:alert(1)\">x</a>",
            "<a href=\"JaVaScRiPt:alert(1)\">x</a>",
            "<a href=\" javascript:alert(1)\">x</a>",
            "<a href=\"data:text/html,<script>alert(1)</script>\">x</a>",
            "<a href=\"vbscript:msgbox(1)\">x</a>",
            "<body onload=alert(1)>",
            "<iframe src=\"https://evil.example\"></iframe>",
            "<object data=x></object><embed src=x>",
            "<form action=\"https://evil.example\"><input name=p></form>",
            "<style>body{background:url(https://evil.example/x)}</style>x",
            "<svg><script>alert(1)</script></svg>",
            "<math><mtext><script>alert(1)</script></mtext></math>",
            "<link rel=stylesheet href=https://evil.example/x.css>",
            "<meta http-equiv=refresh content=\"0;url=https://evil.example\">",
            "<base href=\"https://evil.example/\">",
            "<div style=\"background:url(https://evil.example/x)\">x</div>",
            "<img src=\"//evil.example/x.png\">",
        ] {
            assert_clean(html, false);
            assert_clean(html, true);
        }
    }

    #[test]
    fn safe_formatting_survives() {
        let out = sanitize_html("<p>Hi <b>there</b> <a href=\"https://example.com/x\">link</a></p><ul><li>one</li></ul>", false);
        assert!(
            out.contains("<b>there</b>") && out.contains("<li>one</li>"),
            "{out}"
        );
        assert!(out.contains("href=\"https://example.com/x\""), "{out}");
    }

    #[test]
    fn links_open_elsewhere_without_leaking_the_page() {
        let out = sanitize_html("<a href=\"https://example.com\">x</a>", false);
        assert!(out.contains("target=\"_blank\""), "{out}");
        assert!(
            out.contains("noopener") && out.contains("noreferrer"),
            "{out}"
        );
    }

    #[test]
    fn mailto_links_are_kept_and_relative_links_are_dropped() {
        let out = sanitize_html(
            "<a href=\"mailto:a@b.c\">m</a><a href=\"/admin\">r</a>",
            false,
        );
        assert!(out.contains("mailto:a@b.c"));
        assert!(!out.contains("/admin"), "{out}");
    }

    #[test]
    fn remote_images_are_blocked_until_allowed() {
        let html = "<p>x</p><img src=\"https://tracker.example/pixel.gif\" width=1 height=1>";
        assert!(has_remote_images(html));
        let blocked = sanitize_html(html, false);
        assert!(!blocked.contains("tracker.example"), "{blocked}");
        let allowed = sanitize_html(html, true);
        assert!(
            allowed.contains("https://tracker.example/pixel.gif"),
            "{allowed}"
        );
        assert!(!has_remote_images("<p>no images</p>"));
    }

    #[test]
    fn other_image_sources_are_never_loaded() {
        for src in [
            "cid:logo@x",
            "data:image/png;base64,AAAA",
            "file:///etc/passwd",
            "ftp://x/y.png",
            "//x/y.png",
        ] {
            let out = sanitize_html(&format!("<img src=\"{src}\">"), true);
            assert!(!out.contains(src), "{src} survived: {out}");
        }
    }

    #[test]
    fn address_display_forms() {
        let a = Address {
            name: Some("Ann".into()),
            email: "ann@x.y".into(),
        };
        assert_eq!(
            (a.display().as_str(), a.short().as_str()),
            ("Ann <ann@x.y>", "Ann")
        );
        let b = Address {
            name: None,
            email: "b@x.y".into(),
        };
        assert_eq!(
            (b.display().as_str(), b.short().as_str()),
            ("b@x.y", "b@x.y")
        );
        let c = Address {
            name: Some("c@x.y".into()),
            email: "c@x.y".into(),
        };
        assert_eq!(c.display(), "c@x.y");
    }

    #[test]
    fn flags_round_trip() {
        let f = Flags::from_names(["\\Seen", "\\FLAGGED", "$Junk"]);
        assert!(f.seen && f.flagged && !f.answered);
        assert_eq!(f.names(), ["\\Seen", "\\Flagged"]);
    }
}
