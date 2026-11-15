//! Mail filters and the autoresponder (Phase 8).
//!
//! A user's filters are a list of [`Rule`]s kept in the database. They are turned
//! into a Sieve script ([`sieve`]) that Dovecot runs on delivery, and the same
//! rules can be applied to mail already in the inbox by the fallback
//! [`engine`] when the Sieve server cannot be reached.
//!
//! Everything in this module that builds Sieve text treats user input as data: every
//! string is quoted by [`sieve::quote`], and validation rejects control characters,
//! so a rule cannot inject script text.

pub mod engine;
pub mod sieve;
pub mod store;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Most rules one user may have.
pub const MAX_RULES: usize = 50;
/// Most conditions in one rule.
pub const MAX_CONDITIONS: usize = 10;
/// Most actions in one rule.
pub const MAX_ACTIONS: usize = 5;
/// Longest text a condition may compare against.
pub const MAX_VALUE_CHARS: usize = 200;
/// Longest raw Sieve script a user may save.
pub const MAX_RAW_BYTES: usize = 64 * 1024;

/// The part of a message a condition looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    /// The sender's address.
    From,
    /// A recipient's address (To).
    To,
    /// A copied recipient's address (Cc).
    Cc,
    /// The subject line.
    Subject,
    /// The mailing list identifier (`List-Id`).
    ListId,
}

impl Field {
    /// All fields, in the order the UI lists them.
    pub const ALL: [Field; 5] = [
        Field::From,
        Field::To,
        Field::Cc,
        Field::Subject,
        Field::ListId,
    ];

    /// The stable name used in forms and JSON.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::From => "from",
            Self::To => "to",
            Self::Cc => "cc",
            Self::Subject => "subject",
            Self::ListId => "list_id",
        }
    }

    /// Parses [`Field::as_str`] output.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.as_str() == text)
    }
}

/// How a condition compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    /// The text appears anywhere.
    Contains,
    /// The whole value equals the text.
    Is,
    /// The value starts with the text.
    StartsWith,
    /// The value ends with the text.
    EndsWith,
}

impl Op {
    /// All operators, in the order the UI lists them.
    pub const ALL: [Op; 4] = [Op::Contains, Op::Is, Op::StartsWith, Op::EndsWith];

    /// The stable name used in forms and JSON.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Contains => "contains",
            Self::Is => "is",
            Self::StartsWith => "starts_with",
            Self::EndsWith => "ends_with",
        }
    }

    /// Parses [`Op::as_str`] output.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|o| o.as_str() == text)
    }
}

/// One test on a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Condition {
    /// What to look at.
    pub field: Field,
    /// How to compare.
    pub op: Op,
    /// The text to compare with. Comparison ignores case.
    pub value: String,
}

/// What to do with a message that matched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// File the message in a folder (created if missing).
    MoveTo {
        /// The folder name as IMAP shows it, with `/` between levels.
        folder: String,
    },
    /// Mark the message as read.
    MarkRead,
    /// Throw the message away without telling the sender.
    Discard,
    /// Send a copy to another address.
    Forward {
        /// Where to send it.
        address: String,
    },
    /// Do not run later rules.
    Stop,
}

/// A named set of conditions and actions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// Shown in the list.
    pub name: String,
    /// Disabled rules are kept but not run.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Whether all conditions must match (`true`) or any one of them.
    #[serde(default = "yes")]
    pub match_all: bool,
    /// The tests.
    pub conditions: Vec<Condition>,
    /// What to do on a match.
    pub actions: Vec<Action>,
}

fn yes() -> bool {
    true
}

fn clean(text: &str, what: &str, max: usize) -> Result<()> {
    if text.trim().is_empty() {
        return Err(Error::Validation(format!("{what} must not be empty")));
    }
    if text.chars().count() > max {
        return Err(Error::Validation(format!(
            "{what} is longer than {max} characters"
        )));
    }
    if text.chars().any(char::is_control) {
        return Err(Error::Validation(format!(
            "{what} must not contain control characters"
        )));
    }
    Ok(())
}

/// Whether `folder` is a plausible IMAP folder name.
fn check_folder(folder: &str) -> Result<()> {
    clean(folder, "the folder name", MAX_VALUE_CHARS)?;
    if folder.starts_with('/')
        || folder.ends_with('/')
        || folder.contains("//")
        || folder.split('/').any(|p| p == "." || p == "..")
    {
        return Err(Error::Validation("the folder name is not valid".into()));
    }
    Ok(())
}

/// Whether `address` looks like one plain email address.
///
/// # Errors
/// [`Error::Validation`] when it does not.
pub fn check_address(address: &str) -> Result<()> {
    let ok = address.len() <= 254
        && address.chars().all(|c| {
            !c.is_control()
                && !c.is_whitespace()
                && !matches!(c, '"' | '\\' | '<' | '>' | ',' | ';')
        })
        && address.split_once('@').is_some_and(|(l, d)| {
            !l.is_empty()
                && d.contains('.')
                && !d.contains('@')
                && !d.starts_with('.')
                && !d.ends_with('.')
        });
    if ok {
        Ok(())
    } else {
        Err(Error::Validation("enter one email address".into()))
    }
}

impl Rule {
    /// Checks the rule is complete and safe to turn into a script.
    ///
    /// # Errors
    /// [`Error::Validation`] naming the first problem.
    pub fn validate(&self) -> Result<()> {
        clean(&self.name, "the rule name", 100)?;
        if self.conditions.is_empty() || self.conditions.len() > MAX_CONDITIONS {
            return Err(Error::Validation(format!(
                "a rule needs between 1 and {MAX_CONDITIONS} conditions"
            )));
        }
        if self.actions.is_empty() || self.actions.len() > MAX_ACTIONS {
            return Err(Error::Validation(format!(
                "a rule needs between 1 and {MAX_ACTIONS} actions"
            )));
        }
        for c in &self.conditions {
            clean(&c.value, "a condition's text", MAX_VALUE_CHARS)?;
        }
        for a in &self.actions {
            match a {
                Action::MoveTo { folder } => check_folder(folder)?,
                Action::Forward { address } => check_address(address)?,
                Action::MarkRead | Action::Discard | Action::Stop => {}
            }
        }
        let moves = self
            .actions
            .iter()
            .filter(|a| matches!(a, Action::MoveTo { .. }))
            .count();
        if moves > 1 {
            return Err(Error::Validation(
                "a rule can move a message to one folder only".into(),
            ));
        }
        if moves == 1 && self.actions.contains(&Action::Discard) {
            return Err(Error::Validation(
                "a message cannot be both moved and discarded".into(),
            ));
        }
        Ok(())
    }
}

/// Checks a whole rule list.
///
/// # Errors
/// [`Error::Validation`] for too many rules or the first invalid one.
pub fn validate_rules(rules: &[Rule]) -> Result<()> {
    if rules.len() > MAX_RULES {
        return Err(Error::Validation(format!(
            "at most {MAX_RULES} rules are allowed"
        )));
    }
    for (i, r) in rules.iter().enumerate() {
        r.validate().map_err(|e| match e {
            Error::Validation(m) => Error::Validation(format!("rule {} ({}): {m}", i + 1, r.name)),
            other => other,
        })?;
    }
    Ok(())
}

/// Autoresponder settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Autoresponder {
    /// Whether replies are sent.
    pub enabled: bool,
    /// First day replies are sent (inclusive), or `None` for "from now".
    pub starts_on: Option<chrono::NaiveDate>,
    /// Last day replies are sent (inclusive), or `None` for "until switched off".
    pub ends_on: Option<chrono::NaiveDate>,
    /// Subject of the reply (single line).
    pub subject: String,
    /// Body of the reply.
    pub body: String,
    /// Days before the same sender gets another reply (1 to 30).
    pub days: i32,
}

impl Default for Autoresponder {
    fn default() -> Self {
        Self {
            enabled: false,
            starts_on: None,
            ends_on: None,
            subject: String::new(),
            body: String::new(),
            days: 7,
        }
    }
}

/// Longest reply body.
pub const MAX_BODY_CHARS: usize = 4000;

impl Autoresponder {
    /// Checks the settings. A disabled responder may be incomplete.
    ///
    /// # Errors
    /// [`Error::Validation`] naming the first problem.
    pub fn validate(&self) -> Result<()> {
        if !(1..=30).contains(&self.days) {
            return Err(Error::Validation(
                "the reply interval must be 1 to 30 days".into(),
            ));
        }
        if let (Some(s), Some(e)) = (self.starts_on, self.ends_on) {
            if e < s {
                return Err(Error::Validation(
                    "the end date is before the start date".into(),
                ));
            }
        }
        if self.subject.chars().count() > 200 || self.subject.chars().any(char::is_control) {
            return Err(Error::Validation(
                "the subject must be one line of at most 200 characters".into(),
            ));
        }
        if self.body.chars().count() > MAX_BODY_CHARS
            || self
                .body
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\r' && c != '\t')
        {
            return Err(Error::Validation(format!(
                "the message must be text of at most {MAX_BODY_CHARS} characters"
            )));
        }
        if self.enabled && self.body.trim().is_empty() {
            return Err(Error::Validation(
                "write the message to send before switching the autoresponder on".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule() -> Rule {
        Rule {
            name: "Boss".into(),
            enabled: true,
            match_all: true,
            conditions: vec![Condition {
                field: Field::From,
                op: Op::Contains,
                value: "boss@".into(),
            }],
            actions: vec![Action::MoveTo {
                folder: "Boss".into(),
            }],
        }
    }

    #[test]
    fn a_complete_rule_is_valid() {
        rule().validate().unwrap();
    }

    #[test]
    fn incomplete_or_hostile_rules_are_refused() {
        let mut r = rule();
        r.name = " ".into();
        assert!(r.validate().is_err());
        let mut r = rule();
        r.conditions.clear();
        assert!(r.validate().is_err());
        let mut r = rule();
        r.conditions[0].value = "boss@\n".into();
        assert!(r.validate().is_err(), "control characters are refused");
        for folder in ["", "/x", "x/", "a//b", "../x", "a/./b", "x\ny"] {
            let mut r = rule();
            r.actions = vec![Action::MoveTo {
                folder: folder.into(),
            }];
            assert!(r.validate().is_err(), "{folder:?}");
        }
        for address in [
            "",
            "nobody",
            "a@b",
            "a b@c.de",
            "a@b.de, c@d.de",
            "<a@b.de>",
            "a@@b.de",
        ] {
            let mut r = rule();
            r.actions = vec![Action::Forward {
                address: address.into(),
            }];
            assert!(r.validate().is_err(), "{address:?}");
        }
        let mut r = rule();
        r.actions = vec![Action::MoveTo { folder: "A".into() }, Action::Discard];
        assert!(r.validate().is_err());
    }

    #[test]
    fn rule_lists_are_bounded() {
        let many = vec![rule(); MAX_RULES + 1];
        assert!(validate_rules(&many).is_err());
        let err = validate_rules(&[
            rule(),
            Rule {
                name: "Bad".into(),
                ..rule()
            }
            .tap(|r| r.conditions.clear()),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("rule 2 (Bad)"), "{err}");
    }

    trait Tap: Sized {
        fn tap(mut self, f: impl FnOnce(&mut Self)) -> Self {
            f(&mut self);
            self
        }
    }
    impl Tap for Rule {}

    #[test]
    fn autoresponder_rules() {
        let ok = Autoresponder {
            enabled: true,
            subject: "Away".into(),
            body: "Back soon".into(),
            ..Default::default()
        };
        ok.validate().unwrap();
        assert!(
            Autoresponder {
                body: String::new(),
                ..ok.clone()
            }
            .validate()
            .is_err(),
            "enabled needs a message"
        );
        assert!(Autoresponder {
            enabled: false,
            body: String::new(),
            ..ok.clone()
        }
        .validate()
        .is_ok());
        assert!(Autoresponder {
            days: 0,
            ..ok.clone()
        }
        .validate()
        .is_err());
        assert!(Autoresponder {
            days: 31,
            ..ok.clone()
        }
        .validate()
        .is_err());
        assert!(Autoresponder {
            subject: "a\nb".into(),
            ..ok.clone()
        }
        .validate()
        .is_err());
        let d = |s: &str| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok();
        assert!(Autoresponder {
            starts_on: d("2025-11-10"),
            ends_on: d("2025-11-09"),
            ..ok.clone()
        }
        .validate()
        .is_err());
        assert!(Autoresponder {
            starts_on: d("2025-11-10"),
            ends_on: d("2025-11-10"),
            ..ok
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn rules_survive_json() {
        let json = serde_json::to_string(&vec![rule()]).unwrap();
        assert!(json.contains("\"type\":\"move_to\""));
        let back: Vec<Rule> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, vec![rule()]);
    }
}
