//! Integration tests for the filter store. Requires `DATABASE_URL`.

use rustmail_core::domain::{NewDomain, NewUser, UserRole};
use rustmail_core::filters::store::{FilterMode, FilterStore, Script};
use rustmail_core::filters::{Action, Autoresponder, Condition, Field, Op, Rule};
use rustmail_core::repository::{
    DomainRepository, PgDomainRepository, PgUserRepository, UserRepository,
};
use sqlx::PgPool;
use uuid::Uuid;

async fn user(pool: &PgPool, local: &str) -> Uuid {
    let domains = PgDomainRepository::new(pool.clone());
    let domain = match domains.find_by_name("example.com").await {
        Ok(Some(d)) => d,
        _ => domains
            .create(&NewDomain {
                name: "example.com".into(),
                ..Default::default()
            })
            .await
            .expect("domain"),
    };
    PgUserRepository::new(pool.clone())
        .create(&NewUser {
            domain_id: domain.id,
            local_part: local.into(),
            password_hash: "x".into(),
            display_name: local.into(),
            role: UserRole::User,
            locale: "en".into(),
        })
        .await
        .expect("user")
        .id
}

fn rule(name: &str, from: &str, folder: &str) -> Rule {
    Rule {
        name: name.into(),
        enabled: true,
        match_all: true,
        conditions: vec![Condition {
            field: Field::From,
            op: Op::Contains,
            value: from.into(),
        }],
        actions: vec![
            Action::MoveTo {
                folder: folder.into(),
            },
            Action::MarkRead,
            Action::Stop,
        ],
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn rules_round_trip_in_order_and_replace_each_other(pool: PgPool) {
    let store = FilterStore::new(pool.clone());
    let alice = user(&pool, "alice").await;
    let bob = user(&pool, "bob").await;
    assert!(store.filters(alice).await.unwrap().rules.is_empty());

    let rules = vec![rule("Boss", "boss@", "Boss"), rule("News", "news@", "News")];
    store.save_rules(alice, &rules).await.unwrap();
    store
        .save_rules(bob, &[rule("Bob's", "x@", "X")])
        .await
        .unwrap();
    let saved = store.filters(alice).await.unwrap();
    assert_eq!(
        saved.rules, rules,
        "order, flags and the stop action all survive"
    );
    assert_eq!(saved.mode, FilterMode::Rules);

    store.save_rules(alice, &rules[1..]).await.unwrap();
    assert_eq!(
        store.filters(alice).await.unwrap().rules.len(),
        1,
        "saving replaces"
    );
    assert_eq!(
        store.filters(bob).await.unwrap().rules.len(),
        1,
        "other users are untouched"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn invalid_rules_are_refused_and_change_nothing(pool: PgPool) {
    let store = FilterStore::new(pool.clone());
    let alice = user(&pool, "alice").await;
    store
        .save_rules(alice, &[rule("Boss", "boss@", "Boss")])
        .await
        .unwrap();
    let mut bad = rule("Bad", "x", "Y");
    bad.conditions.clear();
    assert!(store.save_rules(alice, &[bad]).await.is_err());
    assert_eq!(store.filters(alice).await.unwrap().rules.len(), 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn raw_mode_keeps_the_rules_and_can_be_left(pool: PgPool) {
    let store = FilterStore::new(pool.clone());
    let alice = user(&pool, "alice").await;
    store
        .save_rules(alice, &[rule("Boss", "boss@", "Boss")])
        .await
        .unwrap();
    store.save_raw(alice, "keep;\n").await.unwrap();
    let f = store.filters(alice).await.unwrap();
    assert_eq!(
        (f.mode, f.raw_script.as_str(), f.rules.len()),
        (FilterMode::Raw, "keep;\n", 1)
    );
    store.use_rules(alice).await.unwrap();
    let f = store.filters(alice).await.unwrap();
    assert_eq!(
        (f.mode, f.raw_script.as_str()),
        (FilterMode::Rules, "keep;\n"),
        "the script is kept for later"
    );
    assert!(store.save_raw(alice, &"x".repeat(70_000)).await.is_err());
    assert!(store.save_raw(alice, "a\u{0}b").await.is_err());
}

#[sqlx::test(migrations = "../../migrations")]
async fn sync_status_is_reset_by_edits_and_set_by_pushes(pool: PgPool) {
    let store = FilterStore::new(pool.clone());
    let alice = user(&pool, "alice").await;
    store
        .save_rules(alice, &[rule("Boss", "boss@", "Boss")])
        .await
        .unwrap();
    assert_eq!(
        store
            .status(alice, Script::Filters)
            .await
            .unwrap()
            .installed_at,
        None
    );
    store.mark(alice, Script::Filters, None).await.unwrap();
    assert!(store
        .status(alice, Script::Filters)
        .await
        .unwrap()
        .installed_at
        .is_some());
    store
        .mark(alice, Script::Filters, Some("connection refused"))
        .await
        .unwrap();
    let s = store.status(alice, Script::Filters).await.unwrap();
    assert_eq!(
        (s.installed_at, s.error.as_deref()),
        (None, Some("connection refused"))
    );
    store.save_rules(alice, &[]).await.unwrap();
    assert_eq!(
        store.status(alice, Script::Filters).await.unwrap().error,
        None,
        "an edit clears the old error"
    );
    assert_eq!(
        store.status(alice, Script::Vacation).await.unwrap(),
        Default::default(),
        "scripts are tracked separately"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_autoresponder_round_trips(pool: PgPool) {
    let store = FilterStore::new(pool.clone());
    let alice = user(&pool, "alice").await;
    assert!(!store.autoresponder(alice).await.unwrap().settings.enabled);
    let day = |s: &str| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok();
    let settings = Autoresponder {
        enabled: true,
        starts_on: day("2025-12-01"),
        ends_on: day("2025-12-24"),
        subject: "Away".into(),
        body: "Back soon.\nBye".into(),
        days: 3,
    };
    store.save_autoresponder(alice, &settings).await.unwrap();
    assert_eq!(store.autoresponder(alice).await.unwrap().settings, settings);
    let off = Autoresponder {
        enabled: false,
        ..settings.clone()
    };
    store.save_autoresponder(alice, &off).await.unwrap();
    assert_eq!(
        store.autoresponder(alice).await.unwrap().settings,
        off,
        "saving again updates the same row"
    );
    assert!(store
        .save_autoresponder(
            alice,
            &Autoresponder {
                days: 99,
                ..settings
            }
        )
        .await
        .is_err());
}
