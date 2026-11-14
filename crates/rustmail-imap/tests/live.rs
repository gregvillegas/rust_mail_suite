//! Tests against a real Dovecot and Postfix (the dev container, `make mta-up`).
//!
//! Ignored by default. `scripts/webmail-smoke.sh` creates the users and runs them:
//!
//! ```text
//! MAIL_LIVE_USER=alice@x.test MAIL_LIVE_PEER=bob@x.test \
//!   IMAP_PORT=1993 SMTP_PORT=5587 MAIL_TLS_INSECURE=1 MAIL_MASTER_PASSWORD=... \
//!   cargo test -p rustmail-imap --test live -- --ignored --test-threads=1
//! ```

use std::time::Duration;

use rustmail_imap::compose::{build, Draft, OutAttachment};
use rustmail_imap::message::{attachment, parse, Address, Flags};
use rustmail_imap::session::{Change, FlagKind};
use rustmail_imap::{submit, MailConfig, MailError, MailSession};

fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("{key} must be set for the live tests"))
}

fn config() -> MailConfig {
    MailConfig::from_env()
        .expect("valid mail settings")
        .expect("MAIL_MASTER_PASSWORD must be set")
}

fn addr(email: &str) -> Address {
    Address {
        name: None,
        email: email.to_owned(),
    }
}

fn message(from: &str, to: &str, subject: &str, body: &str) -> Vec<u8> {
    format!("From: {from}\r\nTo: {to}\r\nSubject: {subject}\r\nMessage-ID: <{}@live.test>\r\nDate: Tue, 11 Nov 2025 09:30:00 +0000\r\n\r\n{body}\r\n", subject.replace(' ', "-"))
        .into_bytes()
}

/// Waits (up to 60 s) for a message with `subject` to show up in INBOX; returns its UID.
async fn wait_for_subject(config: &MailConfig, user: &str, subject: &str) -> u32 {
    for _ in 0..60 {
        let mut s = MailSession::connect(config, user).await.expect("connect");
        let page = s.list("INBOX", 1, 50).await.expect("list");
        s.close().await;
        if let Some(m) = page.items.iter().find(|m| m.subject == subject) {
            return m.uid;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    panic!("no message with subject {subject:?} arrived for {user}");
}

#[tokio::test]
#[ignore = "needs the dev mail container"]
async fn folders_messages_flags_moves_and_deletes() {
    let cfg = config();
    let user = env("MAIL_LIVE_USER");
    let mut s = MailSession::connect(&cfg, &user)
        .await
        .expect("connect as the user");
    assert_eq!(s.delimiter(), "/");

    // The standard folders exist, in a stable order, with roles.
    let folders = s.folders().await.unwrap();
    assert_eq!(folders[0].name, "INBOX");
    for role in ["Drafts", "Sent", "Trash", "Junk"] {
        assert!(
            folders
                .iter()
                .any(|f| f.name == role && f.special.is_some()),
            "{role}"
        );
    }

    // Folder names with spaces, an ampersand and non-ASCII characters survive the
    // round trip through modified UTF-7.
    // Clear leftovers from an earlier failed run.
    for old in s.folders().await.unwrap() {
        if old.name.starts_with("Tëst & Ünicode") && !old.name.contains('/') {
            s.delete_folder(&old.name).await.unwrap();
        }
    }
    let name = format!("Tëst & Ünicode {}", std::process::id());
    s.create_folder(&name).await.unwrap();
    assert!(
        s.create_folder(&name).await.is_err(),
        "creating twice is refused"
    );
    assert!(s.folders().await.unwrap().iter().any(|f| f.name == name));

    // Store, read back, flag.
    let raw = message(
        "sender@example.org",
        &user,
        "stored directly",
        "Grüße aus dem Test",
    );
    s.append(&name, Flags::default(), &raw).await.unwrap();
    let page = s.list(&name, 1, 25).await.unwrap();
    assert_eq!(page.total, 1);
    let item = &page.items[0];
    assert_eq!(item.subject, "stored directly");
    assert!(!item.flags.seen);

    let opened = s.message(&name, item.uid, true).await.unwrap();
    assert_eq!(
        parse(&opened.bytes).unwrap().text.trim(),
        "Grüße aus dem Test"
    );
    assert!(
        s.list(&name, 1, 25).await.unwrap().items[0].flags.seen,
        "opening marks it read"
    );
    s.set_flag(&name, item.uid, FlagKind::Seen, false)
        .await
        .unwrap();
    s.set_flag(&name, item.uid, FlagKind::Flagged, true)
        .await
        .unwrap();
    let flags = s.list(&name, 1, 25).await.unwrap().items[0].flags;
    assert!(!flags.seen && flags.flagged);

    // A message that is not there is "not found", not a crash.
    assert!(matches!(
        s.message(&name, 999_999, false).await,
        Err(MailError::NotFound)
    ));
    assert!(matches!(
        s.list("No/Such/Folder", 1, 10).await,
        Err(MailError::NotFound)
    ));

    // Move it, then delete it: first to Trash, then for good.
    let target = format!("{name}/Child");
    s.create_folder(&target).await.unwrap();
    s.move_message(&name, item.uid, &target).await.unwrap();
    assert_eq!(s.list(&name, 1, 25).await.unwrap().total, 0);
    let moved = s.list(&target, 1, 25).await.unwrap();
    assert_eq!(moved.total, 1);
    let removed_for_good = s.delete_message(&target, moved.items[0].uid).await.unwrap();
    assert!(!removed_for_good, "first delete goes to the Trash");
    let trash = s.list("Trash", 1, 50).await.unwrap();
    let in_trash = trash
        .items
        .iter()
        .find(|m| m.subject == "stored directly")
        .expect("in Trash");
    assert!(
        s.delete_message("Trash", in_trash.uid).await.unwrap(),
        "deleting from Trash is final"
    );

    // Folder management, and the protections.
    let renamed = format!("{name} renamed");
    s.rename_folder(&name, &renamed).await.unwrap();
    assert!(s.folders().await.unwrap().iter().any(|f| f.name == renamed));
    assert!(
        s.rename_folder("Sent", "Elsewhere").await.is_err(),
        "standard folders are protected"
    );
    assert!(s.delete_folder("INBOX").await.is_err());
    assert!(s.delete_folder("Trash").await.is_err());
    s.delete_folder(&renamed).await.unwrap();
    assert!(
        !s.folders()
            .await
            .unwrap()
            .iter()
            .any(|f| f.name.starts_with(&name)),
        "the folder and its subfolder are both gone"
    );
    s.close().await;
}

#[tokio::test]
#[ignore = "needs the dev mail container"]
async fn the_master_login_refuses_wrong_credentials_and_unknown_users() {
    let cfg = config();
    let mut wrong = cfg.clone();
    wrong.master_password = "not-the-password".into();
    assert!(matches!(
        MailSession::connect(&wrong, &env("MAIL_LIVE_USER")).await,
        Err(MailError::Auth)
    ));
    assert!(MailSession::connect(&cfg, "nobody-here@example.invalid")
        .await
        .is_err());
    // Master syntax in the user name is refused before anything is sent.
    assert!(matches!(
        MailSession::connect(&cfg, "a@b.co*webmail").await,
        Err(MailError::Invalid(_))
    ));
}

#[tokio::test]
#[ignore = "needs the dev mail container"]
async fn sending_delivers_with_attachments_and_keeps_a_copy_in_sent() {
    let cfg = config();
    let user = env("MAIL_LIVE_USER");
    let peer = env("MAIL_LIVE_PEER");
    let subject = format!("live send {}", std::process::id());

    let draft = Draft {
        from: Address {
            name: Some("Live Tester".into()),
            email: user.clone(),
        },
        to: vec![addr(&peer)],
        cc: vec![],
        bcc: vec![],
        subject: subject.clone(),
        body: "Hello from the webmail test.\n".into(),
        in_reply_to: None,
        references: vec![],
        attachments: vec![OutAttachment {
            filename: "note.txt".into(),
            content_type: "text/plain".into(),
            data: b"attached text".to_vec(),
        }],
    };
    let built = build(&draft, &cfg.helo_name).unwrap();
    submit::send(&cfg, &user, &user, &built)
        .await
        .expect("submission accepted");

    // Keep a copy, as the web UI does.
    let mut s = MailSession::connect(&cfg, &user).await.unwrap();
    s.append(
        "Sent",
        Flags {
            seen: true,
            ..Flags::default()
        },
        &built.bytes,
    )
    .await
    .unwrap();
    assert!(s
        .list("Sent", 1, 50)
        .await
        .unwrap()
        .items
        .iter()
        .any(|m| m.subject == subject));
    s.close().await;

    // The recipient gets it (through Postfix, Amavis and Dovecot).
    let uid = wait_for_subject(&cfg, &peer, &subject).await;
    let mut p = MailSession::connect(&cfg, &peer).await.unwrap();
    let raw = p.message("INBOX", uid, false).await.unwrap();
    let parsed = parse(&raw.bytes).unwrap();
    assert_eq!(parsed.from.unwrap().email, user);
    assert!(parsed.text.contains("Hello from the webmail test"));
    assert_eq!(parsed.attachments.len(), 1);
    assert_eq!(attachment(&raw.bytes, 0).unwrap().data, b"attached text");
    p.close().await;
}

#[tokio::test]
#[ignore = "needs the dev mail container"]
async fn postfix_refuses_to_send_as_somebody_else() {
    let cfg = config();
    let user = env("MAIL_LIVE_USER");
    let peer = env("MAIL_LIVE_PEER");
    let draft = Draft {
        from: addr(&peer), // not the signed-in user
        to: vec![addr(&user)],
        cc: vec![],
        bcc: vec![],
        subject: "spoof".into(),
        body: "x".into(),
        in_reply_to: None,
        references: vec![],
        attachments: vec![],
    };
    let built = build(&draft, &cfg.helo_name).unwrap();
    let result = submit::send(&cfg, &user, &peer, &built).await;
    assert!(matches!(result, Err(MailError::Send(_))), "{result:?}");
}

#[tokio::test]
#[ignore = "needs the dev mail container"]
async fn idle_notices_new_mail() {
    let cfg = config();
    let user = env("MAIL_LIVE_USER");
    let watcher = MailSession::connect(&cfg, &user).await.unwrap();

    // Quiet first: nothing happens, and the wait returns on its own.
    let (watcher, change) = watcher
        .wait_for_change("INBOX", Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(change, Change::Quiet);

    // Then deliver something from another session while the first one waits.
    let cfg2 = cfg.clone();
    let user2 = user.clone();
    let delivery = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let mut other = MailSession::connect(&cfg2, &user2).await.unwrap();
        other
            .append(
                "INBOX",
                Flags::default(),
                &message("x@example.org", &user2, "idle wake-up", "hi"),
            )
            .await
            .unwrap();
        other.close().await;
    });
    let (watcher, change) = watcher
        .wait_for_change("INBOX", Duration::from_secs(20))
        .await
        .unwrap();
    delivery.await.unwrap();
    assert_eq!(change, Change::Activity);
    watcher.close().await;
}
