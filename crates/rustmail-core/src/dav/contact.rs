//! Reading vCard (RFC 6350 and the older 3.0 / 2.1 forms) contacts, and building one from
//! the contacts page.

use ical::property::Property;
use ical::VcardParser;

use super::{DavError, DavResult, MAX_OBJECT_BYTES};

/// What the server stores alongside a contact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactInfo {
    /// The `UID`.
    pub uid: String,
    /// The formatted name (`FN`).
    pub display_name: String,
    /// Lower-case words searched by the contacts list: name, organisation, emails, phones.
    pub search_text: String,
}

fn invalid(msg: impl Into<String>) -> DavError {
    DavError::Invalid(msg.into())
}

fn first<'a>(props: &'a [Property], name: &str) -> Option<&'a str> {
    props
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(name))
        .and_then(|p| p.value.as_deref())
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

fn all<'a>(props: &'a [Property], name: &'a str) -> impl Iterator<Item = &'a str> {
    props
        .iter()
        .filter(move |p| p.name.eq_ignore_ascii_case(name))
        .filter_map(|p| p.value.as_deref())
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

/// Undoes vCard text escaping (`\,` `\;` `\n` `\\`).
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Checks `data` and extracts what the server stores alongside it.
///
/// # Errors
/// [`DavError::Invalid`] saying what is wrong.
pub fn parse_contact(data: &str) -> DavResult<ContactInfo> {
    if data.len() > MAX_OBJECT_BYTES {
        return Err(invalid("the contact is too large"));
    }
    if data.contains('\0') {
        return Err(invalid("the contact contains a NUL character"));
    }
    let mut parser = VcardParser::new(data.as_bytes());
    let card = match parser.next() {
        Some(Ok(c)) => c,
        Some(Err(e)) => return Err(invalid(format!("not a valid vCard: {e}"))),
        None => return Err(invalid("not a valid vCard: no BEGIN:VCARD")),
    };
    if parser.next().is_some() {
        return Err(invalid("send one vCard per object"));
    }
    let props = &card.properties;
    let uid = first(props, "UID").ok_or_else(|| invalid("the contact needs a UID"))?;
    if uid.len() > 255 {
        return Err(invalid("the UID is longer than 255 characters"));
    }
    // A name made from N when FN is missing: some clients omit FN in version 3.0.
    let name = first(props, "FN").map(unescape).or_else(|| {
        first(props, "N").map(|n| {
            let mut parts = n.split(';');
            let family = parts.next().unwrap_or("");
            let given = parts.next().unwrap_or("");
            unescape(format!("{given} {family}").trim())
        })
    });
    let display_name = name
        .filter(|n| !n.trim().is_empty())
        .ok_or_else(|| invalid("the contact needs a name (FN)"))?;
    let mut words = vec![display_name.clone()];
    words.extend(all(props, "ORG").map(|v| unescape(&v.replace(';', " "))));
    words.extend(all(props, "NICKNAME").map(unescape));
    words.extend(all(props, "EMAIL").map(unescape));
    words.extend(all(props, "TEL").map(unescape));
    let search_text = words
        .join(" ")
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>();
    Ok(ContactInfo {
        uid: uid.to_owned(),
        display_name: display_name.trim().to_owned(),
        search_text,
    })
}

/// What the contacts page sends for one contact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewContact {
    /// The UID (a fresh one for a new contact).
    pub uid: String,
    /// Full name.
    pub name: String,
    /// Organisation, or empty.
    pub organisation: String,
    /// Email addresses.
    pub emails: Vec<String>,
    /// Phone numbers.
    pub phones: Vec<String>,
    /// Notes, or empty.
    pub notes: String,
}

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

fn fold(line: &str) -> String {
    let mut out = String::new();
    let mut width = 0;
    for c in line.chars() {
        let w = c.len_utf8();
        if width + w > 75 {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(c);
        width += w;
    }
    out
}

/// Writes vCard 3.0 text (the most widely understood version) for a contact made in the page.
///
/// # Errors
/// [`DavError::Invalid`] for a missing name, too many or malformed entries, or long text.
pub fn build_contact(c: &NewContact) -> DavResult<String> {
    let name = c.name.trim();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(invalid("the name must be 1 to 200 characters"));
    }
    if c.emails.len() > 10 || c.phones.len() > 10 {
        return Err(invalid("at most 10 emails and 10 phone numbers"));
    }
    if c.organisation.chars().count() > 200 || c.notes.chars().count() > 4000 {
        return Err(invalid("the organisation or notes are too long"));
    }
    if c.uid.is_empty()
        || c.uid.len() > 255
        || c.uid
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return Err(invalid("the contact id is not valid"));
    }
    for e in &c.emails {
        if crate::filters::check_address(e).is_err() {
            return Err(invalid(format!("“{e}” is not a valid email address")));
        }
    }
    for p in &c.phones {
        if p.chars().count() > 40
            || p.chars()
                .any(|ch| !(ch.is_ascii_digit() || " +-().".contains(ch)))
        {
            return Err(invalid(format!("“{p}” is not a valid phone number")));
        }
    }
    let mut lines = vec![
        "BEGIN:VCARD".to_owned(),
        "VERSION:3.0".to_owned(),
        "PRODID:-//RustMail Suite//Contacts//EN".to_owned(),
        format!("UID:{}", c.uid),
        format!("FN:{}", esc(name)),
    ];
    // N is required by 3.0: use the last word as the family name.
    let (given, family) = name.rsplit_once(' ').map_or((name, ""), |(g, f)| (g, f));
    lines.push(format!("N:{};{};;;", esc(family), esc(given)));
    if !c.organisation.trim().is_empty() {
        lines.push(format!("ORG:{}", esc(c.organisation.trim())));
    }
    lines.extend(
        c.emails
            .iter()
            .filter(|e| !e.trim().is_empty())
            .map(|e| format!("EMAIL;TYPE=INTERNET:{}", esc(e.trim()))),
    );
    lines.extend(
        c.phones
            .iter()
            .filter(|p| !p.trim().is_empty())
            .map(|p| format!("TEL:{}", esc(p.trim()))),
    );
    if !c.notes.trim().is_empty() {
        lines.push(format!("NOTE:{}", esc(c.notes.trim())));
    }
    lines.push("END:VCARD".to_owned());
    Ok(lines
        .iter()
        .map(|l| fold(l))
        .collect::<Vec<_>>()
        .join("\r\n")
        + "\r\n")
}

/// Reads the fields the contacts page edits from stored vCard text. Anything else in the card
/// is left alone by the page (see [`merge_contact`]).
#[must_use]
pub fn read_fields(data: &str) -> Option<NewContact> {
    let card = VcardParser::new(data.as_bytes()).next()?.ok()?;
    let p = &card.properties;
    Some(NewContact {
        uid: first(p, "UID")?.to_owned(),
        name: first(p, "FN").map(unescape)?,
        organisation: first(p, "ORG")
            .map(|o| unescape(&o.replace(';', " ")).trim().to_owned())
            .unwrap_or_default(),
        emails: all(p, "EMAIL").map(unescape).collect(),
        phones: all(p, "TEL").map(unescape).collect(),
        notes: first(p, "NOTE").map(unescape).unwrap_or_default(),
    })
}

/// The page edits a few fields. A card written by a phone may carry photos, addresses and
/// custom fields the page does not show. When such a card is edited here, only the lines the
/// page owns (`FN N ORG EMAIL TEL NOTE`) are replaced and every other line is kept.
///
/// # Errors
/// As [`build_contact`], or [`DavError::Invalid`] if `existing` cannot be read.
pub fn merge_contact(existing: &str, edited: &NewContact) -> DavResult<String> {
    let fresh = build_contact(edited)?;
    let owned = ["FN", "N", "ORG", "EMAIL", "TEL", "NOTE"];
    let keep = |line: &str| {
        let name = line
            .split([':', ';'])
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        !owned.contains(&name.as_str())
            && !["BEGIN", "END", "VERSION", "UID"].contains(&name.as_str())
    };
    // Unfold, so a long line is judged by its start and kept whole.
    let unfolded = existing
        .replace("\r\n ", "")
        .replace("\r\n\t", "")
        .replace("\n ", "");
    let extra: Vec<String> = unfolded
        .lines()
        .filter(|l| !l.trim().is_empty() && keep(l))
        .map(fold)
        .collect();
    if extra.is_empty() {
        return Ok(fresh);
    }
    // Put the kept lines just before END:VCARD.
    let body = fresh
        .trim_end()
        .strip_suffix("END:VCARD")
        .unwrap_or(&fresh)
        .trim_end()
        .to_owned();
    Ok(format!("{body}\r\n{}\r\nEND:VCARD\r\n", extra.join("\r\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARD: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:c1\r\nFN:Ada Lovelace\r\nN:Lovelace;Ada;;;\r\nORG:Analytical Engines;Research\r\nEMAIL;TYPE=WORK:ada@example.org\r\nTEL;TYPE=CELL:+44 20 7946 0000\r\nPHOTO;ENCODING=b;TYPE=JPEG:/9j/4AAQSkZJRgABAQ\r\nX-CUSTOM:keep me\r\nEND:VCARD\r\n";

    #[test]
    fn a_card_is_read() {
        let info = parse_contact(CARD).unwrap();
        assert_eq!(
            (info.uid.as_str(), info.display_name.as_str()),
            ("c1", "Ada Lovelace")
        );
        for word in [
            "ada lovelace",
            "analytical engines",
            "ada@example.org",
            "+44 20 7946 0000",
        ] {
            assert!(
                info.search_text.contains(word),
                "{word} in {}",
                info.search_text
            );
        }
    }

    #[test]
    fn a_missing_fn_falls_back_to_n_and_other_gaps_are_refused() {
        let no_fn = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:c2\r\nN:Hopper;Grace;;;\r\nEND:VCARD\r\n";
        assert_eq!(parse_contact(no_fn).unwrap().display_name, "Grace Hopper");
        for (what, data, expect) in [
            (
                "no uid",
                "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:X\r\nEND:VCARD\r\n".to_owned(),
                "UID",
            ),
            (
                "no name",
                "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:x\r\nEND:VCARD\r\n".to_owned(),
                "name",
            ),
            ("not a card", "hello".to_owned(), "vCard"),
            ("two cards", format!("{CARD}{CARD}"), "one vCard"),
            ("nul", CARD.replace("keep me", "a\0b"), "NUL"),
        ] {
            let err = parse_contact(&data).unwrap_err().to_string();
            assert!(err.contains(expect), "{what}: {err}");
        }
    }

    #[test]
    fn the_page_writes_valid_cards() {
        let c = NewContact {
            uid: "u-1".into(),
            name: "Jean-Luc Picard".into(),
            organisation: "Starfleet; Command".into(),
            emails: vec!["jl@example.org".into()],
            phones: vec!["+33 1 23".into()],
            notes: "Tea, Earl Grey\nHot".into(),
        };
        let text = build_contact(&c).unwrap();
        assert!(
            text.contains("N:Picard;Jean-Luc;;;")
                && text.contains("ORG:Starfleet\\; Command")
                && text.contains("NOTE:Tea\\, Earl Grey\\nHot"),
            "{text}"
        );
        let info = parse_contact(&text).unwrap();
        assert_eq!(info.display_name, "Jean-Luc Picard");
        let back = read_fields(&text).unwrap();
        assert_eq!(
            (
                back.name.as_str(),
                back.emails.clone(),
                back.phones.clone(),
                back.notes.as_str()
            ),
            (
                "Jean-Luc Picard",
                c.emails.clone(),
                c.phones.clone(),
                "Tea, Earl Grey\nHot"
            )
        );
    }

    #[test]
    fn the_page_refuses_bad_input_and_forged_lines() {
        let ok = NewContact {
            uid: "u".into(),
            name: "A B".into(),
            organisation: String::new(),
            emails: vec![],
            phones: vec![],
            notes: String::new(),
        };
        build_contact(&ok).unwrap();
        assert!(build_contact(&NewContact {
            name: " ".into(),
            ..ok.clone()
        })
        .is_err());
        assert!(build_contact(&NewContact {
            emails: vec!["nobody".into()],
            ..ok.clone()
        })
        .is_err());
        assert!(build_contact(&NewContact {
            phones: vec!["call me".into()],
            ..ok.clone()
        })
        .is_err());
        assert!(build_contact(&NewContact {
            emails: vec!["a@b.de".into(); 11],
            ..ok.clone()
        })
        .is_err());
        assert!(build_contact(&NewContact {
            uid: "a b".into(),
            ..ok.clone()
        })
        .is_err());
        let sneaky = NewContact {
            name: "X\r\nEND:VCARD\r\nBEGIN:VCARD".into(),
            ..ok
        };
        let text = build_contact(&sneaky).unwrap();
        assert_eq!(
            text.lines()
                .filter(|l| l.starts_with("BEGIN:VCARD"))
                .count(),
            1,
            "{text}"
        );
        assert_eq!(
            text.lines().filter(|l| l.starts_with("END:VCARD")).count(),
            1,
            "{text}"
        );
        assert!(parse_contact(&text).is_ok());
    }

    #[test]
    fn editing_in_the_page_keeps_what_the_page_does_not_show() {
        let mut c = read_fields(CARD).unwrap();
        c.name = "Augusta Ada King".into();
        c.emails.push("ada@home.example".into());
        let merged = merge_contact(CARD, &c).unwrap();
        assert!(
            merged.contains("FN:Augusta Ada King") && merged.contains("ada@home.example"),
            "{merged}"
        );
        assert!(
            merged.contains("PHOTO;ENCODING=b;TYPE=JPEG:/9j/4AAQSkZJRgABAQ")
                && merged.contains("X-CUSTOM:keep me"),
            "{merged}"
        );
        assert!(
            !merged.contains("FN:Ada Lovelace"),
            "the old name is replaced"
        );
        assert_eq!(merged.matches("UID:").count(), 1);
        assert_eq!(parse_contact(&merged).unwrap().uid, "c1");
    }
}
