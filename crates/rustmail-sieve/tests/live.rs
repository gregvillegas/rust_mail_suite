//! Live test against the dev Dovecot (`make mta-up`). Ignored by default:
//!
//! ```sh
//! LIVE_SIEVE_USER=alice@example.com cargo test -p rustmail-sieve --test live -- --ignored
//! ```
//!
//! The user must exist (create one with `rustmail-cli user create`). The test installs and
//! removes scripts named `live-test-*`; the user's own scripts are not touched.
//! `MAIL_MASTER_PASSWORD` is read from `deploy/dev/mta.env` unless set.

use rustmail_imap::{MailConfig, TlsMode};
use rustmail_sieve::{SieveConfig, SieveError, TlsSession};

fn config() -> SieveConfig {
    let password = std::env::var("MAIL_MASTER_PASSWORD").unwrap_or_else(|_| {
        let env = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../deploy/dev/mta.env"
        ))
        .expect("deploy/dev/mta.env");
        env.lines()
            .find_map(|l| l.strip_prefix("MAIL_MASTER_PASSWORD="))
            .expect("MAIL_MASTER_PASSWORD")
            .to_owned()
    });
    SieveConfig {
        host: "127.0.0.1".into(),
        port: std::env::var("SIEVE_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(4190),
        mail: MailConfig {
            imap_host: "127.0.0.1".into(),
            imap_port: 1993,
            smtp_host: "127.0.0.1".into(),
            smtp_port: 5587,
            tls_server_name: None,
            tls: TlsMode::Insecure,
            master_user: "webmail".into(),
            master_password: password,
            helo_name: "rustmail.test".into(),
        },
    }
}

fn user() -> String {
    std::env::var("LIVE_SIEVE_USER").expect("set LIVE_SIEVE_USER to an existing dev user")
}

#[tokio::test]
#[ignore = "needs the dev mail stack"]
async fn scripts_can_be_stored_checked_listed_activated_and_removed() {
    let mut s = TlsSession::connect(&config(), &user())
        .await
        .expect("login through the master account");
    // A good script is accepted; a bad one is refused with an explanation and not stored.
    s.check("require \"fileinto\";\nfileinto \"Boss\";\n")
        .await
        .expect("valid script");
    let err = s.check("fileintoo \"x\";\n").await.unwrap_err();
    assert!(matches!(err, SieveError::Rejected(_)), "{err:?}");
    assert!(err.to_string().to_lowercase().contains("error"), "{err}");
    let err = s
        .put("live-test-bad", "if true { nonsense; }\n")
        .await
        .unwrap_err();
    assert!(err.is_script_error(), "{err:?}");
    assert!(
        s.get("live-test-bad").await.unwrap().is_none(),
        "a rejected script is not stored"
    );

    // Non-ASCII text survives (the length is in bytes).
    let text = "# Größe – 日本語\nkeep;\n";
    s.put("live-test-a", text).await.unwrap();
    assert_eq!(s.get("live-test-a").await.unwrap().as_deref(), Some(text));
    assert!(s
        .list()
        .await
        .unwrap()
        .iter()
        .any(|x| x.name == "live-test-a"));

    // The include structure RustMail installs: both parts missing is fine (:optional).
    let main = "require [\"include\"];\ninclude :personal :optional \"live-test-missing\";\ninclude :personal :optional \"live-test-a\";\n";
    s.put("live-test-main", main)
        .await
        .expect("optional includes may be missing");

    // Activate ours, then restore whatever was active before (or nothing), and clean up.
    let was_active = s
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|x| x.active)
        .map(|x| x.name);
    s.activate("live-test-main").await.unwrap();
    assert!(s
        .list()
        .await
        .unwrap()
        .iter()
        .any(|x| x.name == "live-test-main" && x.active));
    match was_active.filter(|n| !n.starts_with("live-test-")) {
        Some(name) => s.activate(&name).await.unwrap(),
        None => s.deactivate().await.unwrap(),
    }
    for name in ["live-test-main", "live-test-a"] {
        s.delete(name).await.unwrap();
    }
    s.logout().await;
}

#[tokio::test]
#[ignore = "needs the dev mail stack"]
async fn a_wrong_master_password_is_refused() {
    let mut c = config();
    c.mail.master_password = "definitely-wrong".into();
    assert!(matches!(
        TlsSession::connect(&c, &user()).await,
        Err(SieveError::Auth)
    ));
}
