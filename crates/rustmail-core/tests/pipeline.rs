//! Integration tests for the pipeline store and the quarantine service.
//! Requires `DATABASE_URL` (see `make dev`).
mod common;

use chrono::Utc;
use rustmail_core::domain::UserRole;
use rustmail_core::pipeline::parser::{LogParser, LogTimeZone};
use rustmail_core::pipeline::store::{LogPosition, PipelineStore};
use rustmail_core::pipeline::{EventKind, EventSource, PipelineEvent};
use rustmail_core::quarantine::{
    Actor, QuarantineConfig, QuarantineFilter, QuarantineKind, QuarantineService, QuarantineStatus,
};
use rustmail_core::Error;
use sqlx::PgPool;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

fn blocked(mail_id: &str, recipient: &str, file: &str, verdict: &str) -> PipelineEvent {
    PipelineEvent {
        occurred_at: Utc::now(),
        source: EventSource::Amavis,
        kind: EventKind::Blocked,
        queue_id: Some("QID1".into()),
        mail_id: Some(mail_id.into()),
        verdict: Some(verdict.into()),
        virus_name: (verdict == "INFECTED").then(|| "Eicar-Test".to_owned()),
        spam_score: (verdict == "SPAM").then_some(999.5),
        sender: "x@example.org".into(),
        recipient: recipient.into(),
        status: None,
        detail: None,
        size_bytes: Some(462),
        quarantine_ref: Some(file.into()),
    }
}

fn pos(offset: u64) -> LogPosition {
    LogPosition { inode: 7, offset }
}

#[sqlx::test(migrations = "../../migrations")]
async fn recording_stores_events_the_position_and_quarantine_rows(pool: PgPool) {
    let store = PipelineStore::new(pool.clone());
    assert_eq!(store.position("/log").await.unwrap(), None);
    let summary = store
        .record(
            "/log",
            pos(100),
            &[
                blocked("m1", "a@x.test", "virus-m1", "INFECTED"),
                blocked("m2", "a@x.test", "spam-m2", "SPAM"),
            ],
        )
        .await
        .unwrap();
    assert_eq!((summary.events, summary.quarantined), (2, 2));
    assert_eq!(store.position("/log").await.unwrap(), Some(pos(100)));
    // A later batch moves the position forward.
    store.record("/log", pos(250), &[]).await.unwrap();
    assert_eq!(store.position("/log").await.unwrap(), Some(pos(250)));

    let recent = store.recent(10).await.unwrap();
    assert_eq!(recent.len(), 2);
    assert!(recent
        .iter()
        .any(|e| e.virus_name.as_deref() == Some("Eicar-Test")));
    assert!(recent.iter().any(|e| e.spam_score == Some(999.5)));
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_same_message_is_not_quarantined_twice(pool: PgPool) {
    let store = PipelineStore::new(pool.clone());
    let event = blocked("m1", "a@x.test", "spam-m1", "SPAM");
    store
        .record("/log", pos(1), std::slice::from_ref(&event))
        .await
        .unwrap();
    let again = store.record("/log", pos(2), &[event]).await.unwrap();
    assert_eq!(again.quarantined, 0, "conflict is ignored, not an error");
    let held: i64 = sqlx::query_scalar("SELECT count(*) FROM quarantine_items")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(held, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn unsafe_quarantine_references_are_not_indexed(pool: PgPool) {
    let store = PipelineStore::new(pool.clone());
    let summary = store
        .record(
            "/log",
            pos(1),
            &[
                blocked("m1", "a@x.test", "../../etc/passwd", "SPAM"),
                blocked("m2", "a@x.test", "sub/dir", "SPAM"),
            ],
        )
        .await
        .unwrap();
    assert_eq!(summary.events, 2, "the events are still recorded");
    assert_eq!(summary.quarantined, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn passed_messages_are_events_only(pool: PgPool) {
    let store = PipelineStore::new(pool.clone());
    let mut event = blocked("m1", "a@x.test", "spam-m1", "SPAMMY");
    event.kind = EventKind::Passed;
    let summary = store.record("/log", pos(1), &[event]).await.unwrap();
    assert_eq!((summary.events, summary.quarantined), (1, 0));
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_failed_batch_leaves_nothing_behind(pool: PgPool) {
    let store = PipelineStore::new(pool.clone());
    let mut bad = blocked("m1", "a@x.test", "spam-m1", "SPAM");
    bad.detail = Some("x".repeat(501)); // violates the column's length check
    let result = store
        .record(
            "/log",
            pos(9),
            &[blocked("m0", "b@x.test", "spam-m0", "SPAM"), bad],
        )
        .await;
    assert!(result.is_err());
    assert_eq!(
        store.position("/log").await.unwrap(),
        None,
        "position not advanced"
    );
    assert!(
        store.recent(10).await.unwrap().is_empty(),
        "no partial batch"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn old_events_can_be_pruned(pool: PgPool) {
    let store = PipelineStore::new(pool.clone());
    let mut old = blocked("m1", "a@x.test", "spam-m1", "SPAM");
    old.occurred_at = Utc::now() - chrono::Duration::days(100);
    store
        .record(
            "/log",
            pos(1),
            &[old, blocked("m2", "a@x.test", "spam-m2", "SPAM")],
        )
        .await
        .unwrap();
    assert_eq!(store.prune(90).await.unwrap(), 1);
    assert_eq!(store.recent(10).await.unwrap().len(), 1);
    assert!(store.prune(0).await.is_err());
}

#[sqlx::test(migrations = "../../migrations")]
async fn real_log_lines_flow_from_parser_to_quarantine_index(pool: PgPool) {
    let line = "Nov 13 06:27:59 mail.rustmail.test /usr/sbin/amavisd[555]: (00555-03) Blocked INFECTED (Rustmail.Test.EICAR.UNOFFICIAL) {DiscardedInbound,Quarantined}, [127.0.0.1]:44140 <x@example.org> -> <a@p5.test>,<b@p5.test>, quarantine: virus-R01oJBLIuSX4, Queue-ID: A3D38323855, Message-ID: <1@x>, mail_id: R01oJBLIuSX4, Hits: -, size: 462, 69 ms";
    let events = LogParser::new(Utc::now(), LogTimeZone::Utc).parse(line);
    assert_eq!(events.len(), 2);
    let summary = PipelineStore::new(pool.clone())
        .record("/log", pos(1), &events)
        .await
        .unwrap();
    assert_eq!(
        summary.quarantined, 2,
        "one row per recipient, sharing one file"
    );
    let service = service(&pool, std::path::Path::new("/nonexistent"), "127.0.0.1:1");
    let page = service.list(&QuarantineFilter::default()).await.unwrap();
    assert_eq!(page.total, 2);
    assert!(page.items.iter().all(|i| i.kind == QuarantineKind::Virus));
    assert!(page
        .items
        .iter()
        .all(|i| i.reason == "Rustmail.Test.EICAR.UNOFFICIAL"));
}

// ─── quarantine service ─────────────────────────────────────────────────────

fn service(pool: &PgPool, dir: &std::path::Path, smtp: &str) -> QuarantineService {
    QuarantineService::new(
        pool.clone(),
        QuarantineConfig {
            dir: dir.to_path_buf(),
            reinject_addr: smtp.to_owned(),
            helo: "rustmail.test".into(),
        },
    )
}

async fn actor(pool: &PgPool) -> Actor {
    let domain = common::domain(pool, "admin.test").await;
    let user = common::user(pool, domain.id, "root", UserRole::SuperAdmin).await;
    Actor {
        user_id: user.id,
        email: user.email,
        ip_address: Some("127.0.0.1".into()),
    }
}

const MESSAGE: &str = "Return-Path: <x@example.org>\nDelivered-To: spam-quarantine\nX-Quarantine-ID: <q>\nSubject: hello\n\nbody\n";

/// Accepts one SMTP session and returns everything the client sent.
async fn smtp_sink(accept: bool) -> (String, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let handle = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut read = BufReader::new(read);
        let mut seen = String::new();
        write.write_all(b"220 sink\r\n").await.unwrap();
        let mut line = String::new();
        loop {
            line.clear();
            if read.read_line(&mut line).await.unwrap() == 0 {
                break;
            }
            seen.push_str(&line);
            let up = line.to_ascii_uppercase();
            if up.starts_with("EHLO") || up.starts_with("MAIL") {
                write.write_all(b"250 ok\r\n").await.unwrap();
            } else if up.starts_with("RCPT") {
                let reply: &[u8] = if accept { b"250 ok\r\n" } else { b"550 no\r\n" };
                write.write_all(reply).await.unwrap();
            } else if up.starts_with("DATA") {
                write.write_all(b"354 go\r\n").await.unwrap();
                loop {
                    line.clear();
                    read.read_line(&mut line).await.unwrap();
                    seen.push_str(&line);
                    if line == ".\r\n" {
                        break;
                    }
                }
                write.write_all(b"250 queued\r\n").await.unwrap();
            } else if up.starts_with("QUIT") {
                write.write_all(b"221 bye\r\n").await.unwrap();
                break;
            }
        }
        let mut rest = Vec::new();
        let _ = read.read_to_end(&mut rest).await;
        seen
    });
    (addr, handle)
}

async fn hold(pool: &PgPool, dir: &common::Scratch, name: &str, recipients: &[&str]) {
    std::fs::write(dir.path().join(name), MESSAGE).unwrap();
    let events: Vec<_> = recipients
        .iter()
        .map(|r| blocked("mail1", r, name, "SPAM"))
        .collect();
    PipelineStore::new(pool.clone())
        .record("/log", pos(1), &events)
        .await
        .unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn releasing_sends_the_original_to_that_recipient_only(pool: PgPool) {
    let dir = common::Scratch::new();
    hold(&pool, &dir, "spam-one", &["a@x.test", "b@x.test"]).await;
    let (addr, server) = smtp_sink(true).await;
    let svc = service(&pool, dir.path(), &addr);
    let actor = actor(&pool).await;
    let item = svc
        .list(&QuarantineFilter {
            search: Some("a@x.test".into()),
            ..Default::default()
        })
        .await
        .unwrap()
        .items
        .remove(0);

    let released = svc.release(item.id, &actor).await.unwrap();
    assert_eq!(released.status, QuarantineStatus::Released);
    let seen = server.await.unwrap();
    assert!(seen.contains("RCPT TO:<a@x.test>"), "{seen}");
    assert!(!seen.contains("b@x.test"), "other recipients are untouched");
    assert!(seen.contains("Subject: hello"));
    assert!(
        !seen.contains("Delivered-To"),
        "Amavis's headers are removed"
    );
    // The other recipient's copy is still held, so the file stays.
    assert!(dir.path().join("spam-one").exists());
    // Releasing twice is refused.
    assert!(matches!(
        svc.release(item.id, &actor).await,
        Err(Error::Conflict(_))
    ));
    // The action is in the audit trail.
    let audited: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'quarantine.release'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(audited, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_refused_release_keeps_the_message_held(pool: PgPool) {
    let dir = common::Scratch::new();
    hold(&pool, &dir, "spam-one", &["a@x.test"]).await;
    let (addr, _server) = smtp_sink(false).await;
    let svc = service(&pool, dir.path(), &addr);
    let item = svc
        .list(&QuarantineFilter::default())
        .await
        .unwrap()
        .items
        .remove(0);
    let result = svc.release(item.id, &actor(&pool).await).await;
    assert!(result.is_err());
    assert_eq!(
        svc.get(item.id).await.unwrap().status,
        QuarantineStatus::Held
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn releasing_with_the_mail_server_down_keeps_it_held(pool: PgPool) {
    let dir = common::Scratch::new();
    hold(&pool, &dir, "spam-one", &["a@x.test"]).await;
    let svc = service(&pool, dir.path(), "127.0.0.1:1");
    let item = svc
        .list(&QuarantineFilter::default())
        .await
        .unwrap()
        .items
        .remove(0);
    assert!(svc.release(item.id, &actor(&pool).await).await.is_err());
    assert_eq!(
        svc.get(item.id).await.unwrap().status,
        QuarantineStatus::Held
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_file_is_deleted_only_with_the_last_held_copy(pool: PgPool) {
    let dir = common::Scratch::new();
    hold(&pool, &dir, "spam-one", &["a@x.test", "b@x.test"]).await;
    let svc = service(&pool, dir.path(), "127.0.0.1:1");
    let actor = actor(&pool).await;
    let items = svc.list(&QuarantineFilter::default()).await.unwrap().items;
    svc.delete(items[0].id, &actor).await.unwrap();
    assert!(
        dir.path().join("spam-one").exists(),
        "still needed by the other recipient"
    );
    svc.delete(items[1].id, &actor).await.unwrap();
    assert!(!dir.path().join("spam-one").exists());
    assert!(matches!(
        svc.delete(items[1].id, &actor).await,
        Err(Error::Conflict(_))
    ));
    assert_eq!(
        svc.list(&QuarantineFilter::default()).await.unwrap().total,
        0
    );
    let all = QuarantineFilter {
        status: None,
        ..Default::default()
    };
    assert_eq!(svc.list(&all).await.unwrap().total, 2, "history is kept");
}

#[sqlx::test(migrations = "../../migrations")]
async fn previews_show_headers_and_an_excerpt(pool: PgPool) {
    let dir = common::Scratch::new();
    hold(&pool, &dir, "spam-one", &["a@x.test"]).await;
    let svc = service(&pool, dir.path(), "127.0.0.1:1");
    let item = svc
        .list(&QuarantineFilter::default())
        .await
        .unwrap()
        .items
        .remove(0);
    let preview = svc.preview(item.id).await.unwrap();
    assert!(preview
        .headers
        .iter()
        .any(|(n, v)| n == "Subject" && v == "hello"));
    assert_eq!(preview.excerpt.as_deref(), Some("body\n"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_symlink_in_the_quarantine_directory_is_never_followed(pool: PgPool) {
    let dir = common::Scratch::new();
    let secret = dir.path().join("secret.txt");
    std::fs::write(&secret, "top secret").unwrap();
    std::os::unix::fs::symlink(&secret, dir.path().join("spam-link")).unwrap();
    PipelineStore::new(pool.clone())
        .record(
            "/log",
            pos(1),
            &[blocked("m1", "a@x.test", "spam-link", "SPAM")],
        )
        .await
        .unwrap();
    let svc = service(&pool, dir.path(), "127.0.0.1:1");
    let item = svc
        .list(&QuarantineFilter::default())
        .await
        .unwrap()
        .items
        .remove(0);
    let error = svc.preview(item.id).await.unwrap_err();
    assert!(error.to_string().contains("not a regular file"), "{error}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_missing_file_is_a_clear_error_and_deleting_still_works(pool: PgPool) {
    let dir = common::Scratch::new();
    PipelineStore::new(pool.clone())
        .record(
            "/log",
            pos(1),
            &[blocked("m1", "a@x.test", "spam-gone", "SPAM")],
        )
        .await
        .unwrap();
    let svc = service(&pool, dir.path(), "127.0.0.1:1");
    let item = svc
        .list(&QuarantineFilter::default())
        .await
        .unwrap()
        .items
        .remove(0);
    let error = svc.preview(item.id).await.unwrap_err();
    assert!(error.to_string().contains("missing"), "{error}");
    svc.delete(item.id, &actor(&pool).await).await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn listing_filters_and_counts(pool: PgPool) {
    let store = PipelineStore::new(pool.clone());
    store
        .record(
            "/log",
            pos(1),
            &[
                blocked("m1", "alice@x.test", "virus-1", "INFECTED"),
                blocked("m2", "bob@x.test", "spam-2", "SPAM"),
                blocked("m3", "bob@x.test", "spam-3", "SPAM"),
            ],
        )
        .await
        .unwrap();
    let svc = service(&pool, std::path::Path::new("/x"), "127.0.0.1:1");
    let spam = svc
        .list(&QuarantineFilter {
            kind: Some(QuarantineKind::Spam),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(spam.total, 2);
    let alice = svc
        .list(&QuarantineFilter {
            search: Some("ALICE".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(alice.total, 1);
    // Wildcards in the search text are literal.
    let wild = svc
        .list(&QuarantineFilter {
            search: Some("%".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(wild.total, 0);
    let paged = svc
        .list(&QuarantineFilter {
            limit: 1,
            offset: 2,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!((paged.items.len(), paged.total), (1, 3));
    let counts = svc.counts(None).await.unwrap();
    assert_eq!((counts.virus, counts.spam, counts.other), (1, 2, 0));
}

#[sqlx::test(migrations = "../../migrations")]
async fn retention_purges_old_held_messages_and_their_files(pool: PgPool) {
    let dir = common::Scratch::new();
    std::fs::write(dir.path().join("spam-old"), MESSAGE).unwrap();
    std::fs::write(dir.path().join("spam-new"), MESSAGE).unwrap();
    let mut old = blocked("m1", "a@x.test", "spam-old", "SPAM");
    old.occurred_at = Utc::now() - chrono::Duration::days(40);
    PipelineStore::new(pool.clone())
        .record(
            "/log",
            pos(1),
            &[old, blocked("m2", "a@x.test", "spam-new", "SPAM")],
        )
        .await
        .unwrap();
    let svc = service(&pool, dir.path(), "127.0.0.1:1");
    assert_eq!(svc.purge_older_than(30).await.unwrap(), 1);
    assert!(!dir.path().join("spam-old").exists());
    assert!(dir.path().join("spam-new").exists());
    assert!(svc.purge_older_than(0).await.is_err());
}

#[sqlx::test(migrations = "../../migrations")]
async fn filter_policies_have_a_global_default_and_reject_bad_scopes(pool: PgPool) {
    let default: (String, bool) = sqlx::query_as(
        "SELECT spam_kill_level::text, bypass_virus_checks FROM filter_policies WHERE scope = '@.'",
    )
    .fetch_one(&pool)
    .await
    .map(|(k, b): (String, Option<bool>)| (k, b.unwrap_or(true)))
    .unwrap();
    assert_eq!(default, ("12.00".to_owned(), false));
    for bad in ["example.com", "@", "Alice@Example.com", "a b@c", "@a@b"] {
        let result = sqlx::query("INSERT INTO filter_policies (scope) VALUES ($1)")
            .bind(bad)
            .execute(&pool)
            .await;
        assert!(result.is_err(), "{bad:?} should be rejected");
    }
    for good in ["@example.com", "alice@example.com"] {
        sqlx::query("INSERT INTO filter_policies (scope, spam_kill_level) VALUES ($1, 50)")
            .bind(good)
            .execute(&pool)
            .await
            .unwrap();
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn policies_are_created_updated_partially_and_removed(pool: PgPool) {
    use rustmail_core::policy::{PgPolicyRepository, PolicyUpdate};
    let repo = PgPolicyRepository::new(pool);
    let created = repo
        .set(
            "@Example.com",
            &PolicyUpdate {
                spam_kill_level: Some(50.0),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(created.scope, "@example.com");
    assert_eq!(created.spam_kill_level, Some(50.0));
    assert_eq!(created.spam_tag_level, None, "unset means inherit");

    let changed = repo
        .set(
            "@example.com",
            &PolicyUpdate {
                spam_lover: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(changed.spam_kill_level, Some(50.0), "earlier setting kept");
    assert_eq!(changed.spam_lover, Some(true));

    let scopes: Vec<_> = repo
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.scope)
        .collect();
    assert_eq!(scopes, ["@.", "@example.com"]);
    repo.remove("@example.com").await.unwrap();
    assert!(matches!(
        repo.remove("@example.com").await,
        Err(Error::NotFound { .. })
    ));
    assert!(matches!(repo.remove("@.").await, Err(Error::Validation(_))));
    assert!(repo
        .set("not a scope", &PolicyUpdate::default())
        .await
        .is_err());
}
