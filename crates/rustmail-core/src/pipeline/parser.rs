//! Recognising Postfix and Amavis log lines.
//!
//! Only three kinds of line are interesting; everything else is ignored:
//!
//! * Amavis's one-line verdict: `Passed CLEAN {...}, ... <from> -> <to>, ...`
//!   or `Blocked INFECTED (name) {...}, ...`
//! * Postfix delivery reports (`status=sent|deferred|bounced`) from the final
//!   transports (`lmtp` into Dovecot, `smtp` out to the world)
//! * Postfix refusals during the SMTP conversation (`NOQUEUE: reject:`)
//!
//! Lines are expected in the traditional syslog format (`Nov 13 06:27:59 host
//! ident[pid]: message`) or with an ISO 8601 timestamp, which newer Postfix
//! versions can write. The parser keeps parsed fields only, never the raw line.

use std::sync::LazyLock;

use chrono::{
    DateTime, Datelike, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc,
};
use regex::Regex;

use super::{clip, EventKind, EventSource, PipelineEvent, MAX_DETAIL_CHARS};

/// How to read the time of day in traditional syslog timestamps, which carry
/// neither a year nor a zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogTimeZone {
    /// The log was written on a machine running in UTC (containers, most servers).
    Utc,
    /// The log was written in this machine's local time.
    Local,
}

/// Turns log lines into [`PipelineEvent`]s.
#[derive(Debug, Clone)]
pub struct LogParser {
    now: DateTime<Utc>,
    zone: LogTimeZone,
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
static ISO_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    // 2026-11-13T06:27:59.123456+00:00 host ident[pid]: message
    Regex::new(r"^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})) (\S+) (\S+?)(?:\[\d+\])?: (.*)$")
        .expect("static regex")
});

#[allow(clippy::unwrap_used, clippy::expect_used)]
static SYSLOG_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    // Nov 13 06:27:59 host ident[pid]: message      (the day may be space padded)
    Regex::new(r"^([A-Z][a-z]{2}) +(\d{1,2}) (\d{2}:\d{2}:\d{2}) (\S+) (\S+?)(?:\[\d+\])?: (.*)$")
        .expect("static regex")
});

#[allow(clippy::unwrap_used, clippy::expect_used)]
static AMAVIS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"^\([^)]*\) (?P<act>Passed|Blocked) (?P<verdict>[A-Z][A-Z-]*)",
        r"(?: \((?P<detail>[^)]*)\))?",
        r" \{[^}]*\}, ",
        r"(?:\[[^\]]*\](?::\d+)? )*",
        r"<(?P<from>[^>]*)> -> (?P<to>(?:<[^>]*>,?)+), ",
        r"(?:quarantine: (?P<quarantine>\S+), )?",
        r"(?:Queue-ID: (?P<qid>[^,]+), )?",
        r"(?:Message-ID: (?:<[^>]*>|[^,]*), )?",
        r"mail_id: (?P<mail_id>[^,]+), ",
        r"Hits: (?P<hits>[^,]+), ",
        r"size: (?P<size>\d+)",
    ))
    .expect("static regex")
});

#[allow(clippy::unwrap_used, clippy::expect_used)]
static POSTFIX_DELIVERY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<qid>[0-9A-Za-z]+): to=<(?P<to>[^>]*)>, .*?status=(?P<status>sent|deferred|bounced)(?: \((?P<detail>.*)\))?$")
        .expect("static regex")
});

#[allow(clippy::unwrap_used, clippy::expect_used)]
static POSTFIX_REJECT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^NOQUEUE: reject: \w+ from \S+: (?P<detail>\d{3} .*?); from=<(?P<from>[^>]*)> to=<(?P<to>[^>]*)>")
        .expect("static regex")
});

#[allow(clippy::unwrap_used, clippy::expect_used)]
static ADDRESS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<([^>]*)>").expect("static regex"));

impl LogParser {
    /// Creates a parser. `now` is used to work out the year of syslog timestamps
    /// and may be any time close to when the lines were written.
    #[must_use]
    pub fn new(now: DateTime<Utc>, zone: LogTimeZone) -> Self {
        Self { now, zone }
    }

    /// Parses one line. Returns no events for lines that are not interesting;
    /// one event per recipient otherwise.
    #[must_use]
    pub fn parse(&self, line: &str) -> Vec<PipelineEvent> {
        let Some((when, ident, message)) = self.split_prefix(line) else {
            return Vec::new();
        };
        if ident.contains("amavis") && !ident.starts_with("postfix/") {
            return Self::amavis(when, message);
        }
        match ident {
            "postfix/lmtp" | "postfix/smtp" => Self::delivery(when, message),
            "postfix/smtpd" | "postfix/submission/smtpd" => Self::reject(when, message),
            _ => Vec::new(),
        }
    }

    fn split_prefix<'a>(&self, line: &'a str) -> Option<(DateTime<Utc>, &'a str, &'a str)> {
        if let Some(c) = ISO_PREFIX.captures(line) {
            let when = DateTime::parse_from_rfc3339(c.get(1)?.as_str())
                .ok()?
                .with_timezone(&Utc);
            return Some((when, c.get(3)?.as_str(), c.get(4)?.as_str()));
        }
        let c = SYSLOG_PREFIX.captures(line)?;
        let when = self.syslog_time(c.get(1)?.as_str(), c.get(2)?.as_str(), c.get(3)?.as_str())?;
        Some((when, c.get(5)?.as_str(), c.get(6)?.as_str()))
    }

    /// Works out the full time for a timestamp without year or zone: the most
    /// recent year in which it is not in the future.
    fn syslog_time(&self, month: &str, day: &str, time: &str) -> Option<DateTime<Utc>> {
        let month = match month {
            "Jan" => 1,
            "Feb" => 2,
            "Mar" => 3,
            "Apr" => 4,
            "May" => 5,
            "Jun" => 6,
            "Jul" => 7,
            "Aug" => 8,
            "Sep" => 9,
            "Oct" => 10,
            "Nov" => 11,
            "Dec" => 12,
            _ => return None,
        };
        let day: u32 = day.parse().ok()?;
        let time = NaiveTime::parse_from_str(time, "%H:%M:%S").ok()?;
        let year = self.now.year();
        for candidate in [year, year - 1] {
            // Feb 29 only exists in leap years: try the other year then.
            let Some(date) = NaiveDate::from_ymd_opt(candidate, month, day) else {
                continue;
            };
            let resolved = self.resolve(NaiveDateTime::new(date, time))?;
            // A little slack for clocks that differ by a few minutes.
            if resolved <= self.now + Duration::days(1) {
                return Some(resolved);
            }
        }
        None
    }

    fn resolve(&self, naive: NaiveDateTime) -> Option<DateTime<Utc>> {
        match self.zone {
            LogTimeZone::Utc => Some(Utc.from_utc_datetime(&naive)),
            LogTimeZone::Local => Local
                .from_local_datetime(&naive)
                .earliest()
                .map(|t| t.with_timezone(&Utc)),
        }
    }

    fn amavis(when: DateTime<Utc>, message: &str) -> Vec<PipelineEvent> {
        let Some(c) = AMAVIS.captures(message) else {
            return Vec::new();
        };
        let kind = if &c["act"] == "Blocked" {
            EventKind::Blocked
        } else {
            EventKind::Passed
        };
        let verdict = c["verdict"].to_owned();
        let detail = c.name("detail").map(|d| d.as_str().to_owned());
        // Amavis names the virus in the parentheses after INFECTED.
        let virus_name = (verdict == "INFECTED").then(|| detail.clone()).flatten();
        let spam_score = c["hits"].trim().parse::<f64>().ok();
        let size_bytes = c["size"].parse::<i64>().ok();
        let sender = clip(&c["from"], 320);
        let quarantine_ref = c.name("quarantine").map(|q| q.as_str().to_owned());
        ADDRESS
            .captures_iter(&c["to"])
            .map(|to| PipelineEvent {
                occurred_at: when,
                source: EventSource::Amavis,
                kind,
                queue_id: c.name("qid").map(|q| q.as_str().to_owned()),
                mail_id: Some(c["mail_id"].to_owned()),
                verdict: Some(verdict.clone()),
                virus_name: virus_name.clone(),
                spam_score,
                sender: sender.clone(),
                recipient: clip(&to[1], 320),
                status: None,
                detail: if virus_name.is_some() {
                    None
                } else {
                    detail.as_deref().map(|d| clip(d, MAX_DETAIL_CHARS))
                },
                size_bytes,
                quarantine_ref: quarantine_ref.clone(),
            })
            .collect()
    }

    fn delivery(when: DateTime<Utc>, message: &str) -> Vec<PipelineEvent> {
        let Some(c) = POSTFIX_DELIVERY.captures(message) else {
            return Vec::new();
        };
        let status = c["status"].to_owned();
        // A successful delivery's reply text is just noise; failures explain why.
        let detail = if status == "sent" {
            None
        } else {
            c.name("detail").map(|d| clip(d.as_str(), MAX_DETAIL_CHARS))
        };
        vec![PipelineEvent {
            occurred_at: when,
            source: EventSource::Postfix,
            kind: EventKind::Delivery,
            queue_id: Some(c["qid"].to_owned()),
            mail_id: None,
            verdict: None,
            virus_name: None,
            spam_score: None,
            sender: String::new(),
            recipient: clip(&c["to"], 320),
            status: Some(status),
            detail,
            size_bytes: None,
            quarantine_ref: None,
        }]
    }

    fn reject(when: DateTime<Utc>, message: &str) -> Vec<PipelineEvent> {
        let Some(c) = POSTFIX_REJECT.captures(message) else {
            return Vec::new();
        };
        vec![PipelineEvent {
            occurred_at: when,
            source: EventSource::Postfix,
            kind: EventKind::Reject,
            queue_id: None,
            mail_id: None,
            verdict: None,
            virus_name: None,
            spam_score: None,
            sender: clip(&c["from"], 320),
            recipient: clip(&c["to"], 320),
            status: None,
            detail: Some(clip(&c["detail"], MAX_DETAIL_CHARS)),
            size_bytes: None,
            quarantine_ref: None,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parser() -> LogParser {
        LogParser::new(
            Utc.with_ymd_and_hms(2026, 11, 14, 12, 0, 0)
                .single()
                .expect("date"),
            LogTimeZone::Utc,
        )
    }

    // The lines below were captured from Debian 12 (Amavis 2.13, Postfix 3.7).
    const INFECTED: &str = "Nov 13 06:27:59 mail.rustmail.test /usr/sbin/amavisd[555]: (00555-03) Blocked INFECTED (Rustmail.Test.EICAR.UNOFFICIAL) {DiscardedInbound,Quarantined}, [127.0.0.1]:44140 <x@example.org> -> <a@p5.test>, quarantine: virus-R01oJBLIuSX4, Queue-ID: A3D38323855, Message-ID: <20261113062759.000596@mail.rustmail.test>, mail_id: R01oJBLIuSX4, Hits: -, size: 462, 69 ms";
    const SPAM: &str = "Nov 13 06:27:59 mail.rustmail.test /usr/sbin/amavisd[556]: (00556-03) Blocked SPAM {DiscardedInbound,Quarantined}, [127.0.0.1]:44150 <x@example.org> -> <a@p5.test>, quarantine: spam-gQjyHtQaURcD, Queue-ID: B517B323860, Message-ID: <20261113062759.000597@mail.rustmail.test>, mail_id: gQjyHtQaURcD, Hits: 999.001, size: 462, 122 ms";
    const CLEAN: &str = "Nov 13 06:27:59 mail.rustmail.test /usr/sbin/amavisd[555]: (00555-04) Passed CLEAN {RelayedInbound}, [127.0.0.1]:44162 <x@example.org> -> <a@p5.test>, Queue-ID: C08D1323855, Message-ID: <20261113062759.000599@mail.rustmail.test>, mail_id: YNN-PL07-Wbe, Hits: -0.999, size: 405, queued_as: DDB09323860, 119 ms";
    const TEMPFAIL: &str = "Nov 13 06:24:23 mail.rustmail.test /usr/sbin/amavisd[446]: (00446-01) Blocked MTA-BLOCKED {TempFailedInbound}, [127.0.0.1]:50792 <x@example.org> -> <a@p5.test>, Queue-ID: EACF9323851, Message-ID: <20261113062421.000460@mail.rustmail.test>, mail_id: LhrTOKd4AwPx, Hits: -0.999, size: 462, 1144 ms";
    const SENT: &str = "Nov 13 06:26:26 mail postfix/lmtp[719]: 704E5323891: to=<a@p5.test>, relay=mail.rustmail.test[private/dovecot-lmtp], delay=0.11, delays=0.04/0/0.01/0.05, dsn=2.0.0, status=sent (250 2.0.0 <a@p5.test> IRNKHpKt9mrRAgAA+MOpmA Saved)";
    const REJECT: &str = "Nov 13 06:30:01 mail postfix/smtpd[565]: NOQUEUE: reject: RCPT from unknown[172.18.0.1]: 550 5.1.1 <nobody@p5.test>: Recipient address rejected: User unknown in virtual mailbox table; from=<s@example.org> to=<nobody@p5.test> proto=ESMTP helo=<h>";

    #[test]
    fn an_infected_message_names_the_virus_and_its_quarantine_file() {
        let events = parser().parse(INFECTED);
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(
            (e.source, e.kind),
            (EventSource::Amavis, EventKind::Blocked)
        );
        assert_eq!(e.verdict.as_deref(), Some("INFECTED"));
        assert_eq!(
            e.virus_name.as_deref(),
            Some("Rustmail.Test.EICAR.UNOFFICIAL")
        );
        assert_eq!(e.quarantine_ref.as_deref(), Some("virus-R01oJBLIuSX4"));
        assert_eq!(e.mail_id.as_deref(), Some("R01oJBLIuSX4"));
        assert_eq!(e.queue_id.as_deref(), Some("A3D38323855"));
        assert_eq!(
            (e.sender.as_str(), e.recipient.as_str()),
            ("x@example.org", "a@p5.test")
        );
        assert_eq!(e.spam_score, None, "'-' means no score");
        assert_eq!(e.size_bytes, Some(462));
        assert_eq!(e.occurred_at.to_rfc3339(), "2026-11-13T06:27:59+00:00");
    }

    #[test]
    fn a_spam_message_has_a_score_and_a_quarantine_file() {
        let e = &parser().parse(SPAM)[0];
        assert_eq!(e.verdict.as_deref(), Some("SPAM"));
        assert_eq!(e.spam_score, Some(999.001));
        assert_eq!(e.quarantine_ref.as_deref(), Some("spam-gQjyHtQaURcD"));
        assert_eq!(e.virus_name, None);
    }

    #[test]
    fn a_clean_message_passes_without_quarantine() {
        let e = &parser().parse(CLEAN)[0];
        assert_eq!(
            (e.kind, e.verdict.as_deref()),
            (EventKind::Passed, Some("CLEAN"))
        );
        assert_eq!(e.spam_score, Some(-0.999));
        assert_eq!(e.quarantine_ref, None);
        assert_eq!(e.mail_id.as_deref(), Some("YNN-PL07-Wbe"));
    }

    #[test]
    fn a_temporary_failure_is_blocked_but_not_quarantined() {
        let e = &parser().parse(TEMPFAIL)[0];
        assert_eq!(
            (e.kind, e.verdict.as_deref()),
            (EventKind::Blocked, Some("MTA-BLOCKED"))
        );
        assert_eq!(e.quarantine_ref, None);
    }

    #[test]
    fn every_recipient_gets_its_own_event() {
        let line = CLEAN.replace("-> <a@p5.test>", "-> <a@p5.test>,<b@p5.test>,<c@p5.test>");
        let events = parser().parse(&line);
        let recipients: Vec<_> = events.iter().map(|e| e.recipient.as_str()).collect();
        assert_eq!(recipients, ["a@p5.test", "b@p5.test", "c@p5.test"]);
    }

    #[test]
    fn a_null_sender_is_kept_as_empty() {
        let line = CLEAN.replace("<x@example.org>", "<>");
        assert_eq!(parser().parse(&line)[0].sender, "");
    }

    #[test]
    fn postfix_delivery_reports_status() {
        let e = &parser().parse(SENT)[0];
        assert_eq!(
            (e.source, e.kind),
            (EventSource::Postfix, EventKind::Delivery)
        );
        assert_eq!(e.status.as_deref(), Some("sent"));
        assert_eq!(e.queue_id.as_deref(), Some("704E5323891"));
        assert_eq!(e.recipient, "a@p5.test");
        assert_eq!(e.detail, None, "success text is noise");
    }

    #[test]
    fn a_deferral_keeps_the_reason() {
        let line = "Nov 13 06:26:26 mail postfix/smtp[1]: ABC123: to=<a@b.example>, relay=none, delay=1, dsn=4.4.1, status=deferred (connect to b.example[1.2.3.4]:25: Connection refused)";
        let e = &parser().parse(line)[0];
        assert_eq!(e.status.as_deref(), Some("deferred"));
        assert_eq!(
            e.detail.as_deref(),
            Some("connect to b.example[1.2.3.4]:25: Connection refused")
        );
    }

    #[test]
    fn rejected_recipients_are_recorded_without_the_client_address() {
        let e = &parser().parse(REJECT)[0];
        assert_eq!(e.kind, EventKind::Reject);
        assert_eq!(
            (e.sender.as_str(), e.recipient.as_str()),
            ("s@example.org", "nobody@p5.test")
        );
        assert!(e
            .detail
            .as_deref()
            .is_some_and(|d| d.starts_with("550 5.1.1")));
        assert!(
            !format!("{e:?}").contains("172.18.0.1"),
            "client address is not stored"
        );
    }

    #[test]
    fn hand_offs_between_postfix_and_amavis_are_ignored() {
        let line = "Nov 13 06:24:22 mail postfix/amavis/smtp[468]: 06E39323881: to=<a@p5.test>, relay=127.0.0.1[127.0.0.1]:10024, delay=0.14, dsn=2.7.0, status=sent (250 2.7.0 Ok, discarded)";
        assert!(parser().parse(line).is_empty());
    }

    #[test]
    fn unrelated_lines_are_ignored() {
        for line in [
            "",
            "garbage",
            "Nov 13 06:21:26 mail postfix/smtpd[565]: connect from localhost[127.0.0.1]",
            "Nov 13 06:21:26 mail.rustmail.test /usr/sbin/amavisd[556]: (00556-01) Checking: 40PQpFlvzUix [127.0.0.1] <x@example.org> -> <a@p5.test>",
            "Nov 13 06:21:26 mail dovecot: imap(a@p5.test): Logged out",
        ] {
            assert!(parser().parse(line).is_empty(), "{line:?}");
        }
    }

    #[test]
    fn iso_timestamps_are_understood() {
        let line = SENT.replacen("Nov 13 06:26:26", "2026-11-13T06:26:26.123456+01:00", 1);
        let e = &parser().parse(&line)[0];
        assert_eq!(
            e.occurred_at.to_rfc3339(),
            "2026-11-13T05:26:26.123456+00:00"
        );
    }

    #[test]
    fn a_december_line_read_in_january_belongs_to_last_year() {
        let january = LogParser::new(
            Utc.with_ymd_and_hms(2027, 1, 2, 0, 0, 0)
                .single()
                .expect("date"),
            LogTimeZone::Utc,
        );
        let line = SENT.replacen("Nov 13", "Dec 31", 1);
        assert_eq!(
            january.parse(&line)[0].occurred_at.to_rfc3339(),
            "2026-12-31T06:26:26+00:00"
        );
    }

    #[test]
    fn space_padded_days_are_understood() {
        let line = SENT.replacen("Nov 13", "Nov  3", 1);
        assert_eq!(
            parser().parse(&line)[0].occurred_at.to_rfc3339(),
            "2026-11-03T06:26:26+00:00"
        );
    }

    #[test]
    fn hostile_text_cannot_overflow_stored_fields() {
        let line = SENT.replace("Connection", "x");
        let long = format!(
            "Nov 13 06:26:26 mail postfix/smtp[1]: ABC: to=<a@b.c>, relay=none, status=deferred ({})",
            "z".repeat(5000)
        );
        let e = &parser().parse(&long)[0];
        assert!(e
            .detail
            .as_deref()
            .is_some_and(|d| d.chars().count() <= MAX_DETAIL_CHARS));
        let _ = line;
    }
}
