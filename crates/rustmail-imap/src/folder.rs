//! Folders (IMAP mailboxes) as the webmail shows them.

use serde::Serialize;

use crate::error::{MailError, MailResult};

/// The longest folder name accepted.
pub const MAX_FOLDER_NAME: usize = 200;

/// What a folder is for, as announced by the server (RFC 6154 `SPECIAL-USE`)
/// or implied by its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SpecialUse {
    /// The inbox.
    Inbox,
    /// Unsent drafts.
    Drafts,
    /// Sent mail.
    Sent,
    /// Deleted mail.
    Trash,
    /// Spam.
    Junk,
    /// Archive.
    Archive,
}

impl SpecialUse {
    /// Sort order in the folder list: the usual ones first, in a fixed order.
    #[must_use]
    pub fn rank(self) -> u8 {
        match self {
            Self::Inbox => 0,
            Self::Drafts => 1,
            Self::Sent => 2,
            Self::Archive => 3,
            Self::Junk => 4,
            Self::Trash => 5,
        }
    }

    /// The key used in translations (`mail-folder-<key>`).
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Inbox => "inbox",
            Self::Drafts => "drafts",
            Self::Sent => "sent",
            Self::Trash => "trash",
            Self::Junk => "junk",
            Self::Archive => "archive",
        }
    }
}

/// One folder with its counters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Folder {
    /// Full name, decoded (for example `Work/Projects`).
    pub name: String,
    /// Hierarchy separator used by the server.
    pub delimiter: Option<String>,
    /// Role of the folder, if it has a standard one.
    pub special: Option<SpecialUse>,
    /// Number of messages.
    pub total: u32,
    /// Number of unread messages.
    pub unread: u32,
    /// Whether messages can be stored in it (`\Noselect` folders are only containers).
    pub selectable: bool,
}

impl Folder {
    /// The last part of the name, for indented display.
    #[must_use]
    pub fn leaf(&self) -> &str {
        match &self.delimiter {
            Some(d) if !d.is_empty() => self.name.rsplit(d.as_str()).next().unwrap_or(&self.name),
            _ => &self.name,
        }
    }

    /// How deeply the folder is nested.
    #[must_use]
    pub fn depth(&self) -> usize {
        match &self.delimiter {
            Some(d) if !d.is_empty() => self.name.matches(d.as_str()).count(),
            _ => 0,
        }
    }
}

/// Guesses the role from the name, for servers that do not announce one.
#[must_use]
pub fn special_from_name(name: &str) -> Option<SpecialUse> {
    match name.to_ascii_lowercase().as_str() {
        "inbox" => Some(SpecialUse::Inbox),
        "drafts" | "draft" => Some(SpecialUse::Drafts),
        "sent" | "sent items" | "sent messages" => Some(SpecialUse::Sent),
        "trash" | "deleted items" | "deleted messages" => Some(SpecialUse::Trash),
        "junk" | "spam" | "junk e-mail" => Some(SpecialUse::Junk),
        "archive" | "archives" => Some(SpecialUse::Archive),
        _ => None,
    }
}

/// Checks a folder name chosen by a user.
///
/// Names are limited to sensible text: no control characters, no IMAP wildcards
/// (`*`, `%`), no leading or trailing separator, no empty or `.`/`..` parts.
///
/// # Errors
/// [`MailError::Invalid`] with a message fit for showing to the user.
pub fn validate_name(name: &str, delimiter: &str) -> MailResult<()> {
    let bad = |why: &str| Err(MailError::Invalid(why.to_owned()));
    if name.trim().is_empty() {
        return bad("the folder name is empty");
    }
    if name.chars().count() > MAX_FOLDER_NAME {
        return bad("the folder name is too long");
    }
    if name != name.trim() {
        return bad("the folder name must not start or end with spaces");
    }
    if name.chars().any(|c| c.is_control() || matches!(c, '*' | '%' | '\\' | '"')) {
        return bad("the folder name contains a character that is not allowed");
    }
    if name.eq_ignore_ascii_case("inbox") {
        return bad("INBOX already exists");
    }
    for part in name.split(delimiter) {
        if part.is_empty() || part == "." || part == ".." || part != part.trim() {
            return bad("the folder name has an empty or invalid part");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nesting_is_derived_from_the_delimiter() {
        let f = Folder {
            name: "Work/Projects/2024".into(),
            delimiter: Some("/".into()),
            special: None,
            total: 0,
            unread: 0,
            selectable: true,
        };
        assert_eq!((f.leaf(), f.depth()), ("2024", 2));
        let flat = Folder { delimiter: None, ..f };
        assert_eq!((flat.leaf(), flat.depth()), ("Work/Projects/2024", 0));
    }

    #[test]
    fn roles_are_recognised_by_name() {
        assert_eq!(special_from_name("INBOX"), Some(SpecialUse::Inbox));
        assert_eq!(special_from_name("Sent Items"), Some(SpecialUse::Sent));
        assert_eq!(special_from_name("Spam"), Some(SpecialUse::Junk));
        assert_eq!(special_from_name("Projects"), None);
    }

    #[test]
    fn folder_names_are_validated() {
        for ok in ["Projects", "Work/Projects", "Entwürfe", "Q&A", "2024 plans"] {
            assert!(validate_name(ok, "/").is_ok(), "{ok}");
        }
        for bad in [
            "", " ", " x", "x ", "a/", "/a", "a//b", "../x", "a/./b", "a*b", "a%b", "a\\b", "a\"b", "a\r\nb",
            "INBOX", "inbox", &"x".repeat(MAX_FOLDER_NAME + 1),
        ] {
            assert!(validate_name(bad, "/").is_err(), "{bad:?}");
        }
    }
}
