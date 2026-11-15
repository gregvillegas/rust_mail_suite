//! The fallback rules engine.
//!
//! Dovecot normally applies a user's filters while it delivers a message. When the
//! Sieve server cannot be reached the rules cannot be installed, so the web UI can
//! apply them to mail already in the inbox instead. This engine decides what should
//! happen to one message; the caller carries it out over IMAP.
//!
//! It only does what is safe to do after the fact: **move to a folder** and **mark
//! read**. `Discard` and `Forward` are reported as skipped, because deleting or
//! re-sending old mail on a guess would be worse than leaving it.

use super::{Action, Condition, Field, Op, Rule};

/// What the engine needs to know about a message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    /// Sender addresses (`From`).
    pub from: Vec<String>,
    /// `To` addresses.
    pub to: Vec<String>,
    /// `Cc` addresses.
    pub cc: Vec<String>,
    /// The subject, decoded.
    pub subject: String,
    /// The `List-Id` header, if any.
    pub list_id: String,
}

/// What to do with one message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Folder to move it to.
    pub move_to: Option<String>,
    /// Whether to mark it read.
    pub mark_read: bool,
    /// Actions the engine will not carry out (discard, forward), by rule name.
    pub skipped: Vec<String>,
}

fn compare(op: Op, haystack: &str, needle: &str) -> bool {
    let (h, n) = (haystack.to_lowercase(), needle.to_lowercase());
    match op {
        Op::Contains => h.contains(&n),
        Op::Is => h == n,
        Op::StartsWith => h.starts_with(&n),
        Op::EndsWith => h.ends_with(&n),
    }
}

fn matches_condition(c: &Condition, facts: &Facts) -> bool {
    match c.field {
        Field::From => facts.from.iter().any(|a| compare(c.op, a, &c.value)),
        Field::To => facts.to.iter().any(|a| compare(c.op, a, &c.value)),
        Field::Cc => facts.cc.iter().any(|a| compare(c.op, a, &c.value)),
        Field::Subject => compare(c.op, &facts.subject, &c.value),
        Field::ListId => compare(c.op, &facts.list_id, &c.value),
    }
}

/// Whether `rule` applies to a message with these facts.
#[must_use]
pub fn rule_matches(rule: &Rule, facts: &Facts) -> bool {
    rule.enabled
        && !rule.conditions.is_empty()
        && if rule.match_all {
            rule.conditions.iter().all(|c| matches_condition(c, facts))
        } else {
            rule.conditions.iter().any(|c| matches_condition(c, facts))
        }
}

/// Runs the rules in order against one message. The first rule that names a
/// folder decides the folder; `Stop` ends the run.
#[must_use]
pub fn evaluate(rules: &[Rule], facts: &Facts) -> Outcome {
    let mut out = Outcome::default();
    for rule in rules.iter().filter(|r| rule_matches(r, facts)) {
        for action in &rule.actions {
            match action {
                Action::MoveTo { folder } => {
                    if out.move_to.is_none() {
                        out.move_to = Some(folder.clone());
                    }
                }
                Action::MarkRead => out.mark_read = true,
                Action::Discard | Action::Forward { .. } => out.skipped.push(rule.name.clone()),
                Action::Stop => return out,
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(name: &str, field: Field, op: Op, value: &str, actions: Vec<Action>) -> Rule {
        Rule {
            name: name.into(),
            enabled: true,
            match_all: true,
            conditions: vec![Condition {
                field,
                op,
                value: value.into(),
            }],
            actions,
        }
    }

    fn from(addr: &str) -> Facts {
        Facts {
            from: vec![addr.into()],
            subject: "Quarterly plan".into(),
            ..Default::default()
        }
    }

    #[test]
    fn from_boss_goes_to_the_boss_folder() {
        let rules = [rule(
            "Boss",
            Field::From,
            Op::Contains,
            "boss@",
            vec![Action::MoveTo {
                folder: "Boss".into(),
            }],
        )];
        assert_eq!(
            evaluate(&rules, &from("Boss@Example.com"))
                .move_to
                .as_deref(),
            Some("Boss")
        );
        assert_eq!(
            evaluate(&rules, &from("friend@example.com")),
            Outcome::default()
        );
    }

    #[test]
    fn operators_ignore_case() {
        let f = Facts {
            subject: "[Dev] Weekly Report".into(),
            ..Default::default()
        };
        for (op, v, expect) in [
            (Op::Contains, "weekly", true),
            (Op::Is, "[dev] weekly report", true),
            (Op::Is, "weekly", false),
            (Op::StartsWith, "[DEV]", true),
            (Op::EndsWith, "report", true),
            (Op::EndsWith, "[dev]", false),
        ] {
            let r = rule("r", Field::Subject, op, v, vec![Action::MarkRead]);
            assert_eq!(rule_matches(&r, &f), expect, "{op:?} {v}");
        }
    }

    #[test]
    fn all_versus_any() {
        let mut r = rule("r", Field::From, Op::Contains, "a@", vec![Action::MarkRead]);
        r.conditions.push(Condition {
            field: Field::Subject,
            op: Op::Contains,
            value: "nope".into(),
        });
        assert!(!rule_matches(&r, &from("a@x.org")));
        r.match_all = false;
        assert!(rule_matches(&r, &from("a@x.org")));
        r.enabled = false;
        assert!(
            !rule_matches(&r, &from("a@x.org")),
            "disabled rules never match"
        );
    }

    #[test]
    fn first_folder_wins_and_stop_ends_the_run() {
        let rules = [
            rule(
                "one",
                Field::From,
                Op::Contains,
                "a@",
                vec![
                    Action::MoveTo {
                        folder: "One".into(),
                    },
                    Action::Stop,
                ],
            ),
            rule(
                "two",
                Field::From,
                Op::Contains,
                "a@",
                vec![
                    Action::MoveTo {
                        folder: "Two".into(),
                    },
                    Action::MarkRead,
                ],
            ),
        ];
        let out = evaluate(&rules, &from("a@x.org"));
        assert_eq!(
            (out.move_to.as_deref(), out.mark_read),
            (Some("One"), false)
        );
        let rules = [rules[0].clone().tap_no_stop(), rules[1].clone()];
        let out = evaluate(&rules, &from("a@x.org"));
        assert_eq!((out.move_to.as_deref(), out.mark_read), (Some("One"), true));
    }

    trait NoStop {
        fn tap_no_stop(self) -> Self;
    }
    impl NoStop for Rule {
        fn tap_no_stop(mut self) -> Self {
            self.actions.retain(|a| *a != Action::Stop);
            self
        }
    }

    #[test]
    fn destructive_actions_are_skipped_not_run() {
        let rules = [rule(
            "Bin",
            Field::From,
            Op::Contains,
            "spam@",
            vec![Action::Discard],
        )];
        let out = evaluate(&rules, &from("spam@x.org"));
        assert_eq!(out.skipped, vec!["Bin".to_owned()]);
        assert_eq!(out.move_to, None);
    }
}
