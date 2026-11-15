//! Integration tests for calendars, contacts, sharing, free/busy and app passwords.
//! Requires `DATABASE_URL`.

use chrono::{DateTime, Duration, Utc};
use rustmail_core::dav::store::{
    Access, Busy, CollectionUpdate, DavStore, GranteeRef, Kind, Precondition,
};
use rustmail_core::dav::tokens::{AppPasswords, MAX_ACTIVE};
use rustmail_core::dav::DavError;
use rustmail_core::domain::{NewDomain, NewUser, UserRole};
use rustmail_core::repository::{
    DomainRepository, PgDomainRepository, PgUserRepository, UserRepository,
};
use sqlx::PgPool;
use uuid::Uuid;

struct World {
    store: DavStore,
    alice: Uuid,
    bob: Uuid,
    carol: Uuid,
    zed: Uuid,
}

async fn world(pool: &PgPool) -> World {
    let domains = PgDomainRepository::new(pool.clone());
    let users = PgUserRepository::new(pool.clone());
    let example = domains
        .create(&NewDomain {
            name: "example.com".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let other = domains
        .create(&NewDomain {
            name: "other.org".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let make = |domain, local: &'static str| {
        let users = users.clone();
        async move {
            users
                .create(&NewUser {
                    domain_id: domain,
                    local_part: local.into(),
                    password_hash: "x".into(),
                    display_name: local.into(),
                    role: UserRole::User,
                    locale: "en".into(),
                })
                .await
                .unwrap()
                .id
        }
    };
    let alice = make(example.id, "alice").await;
    let bob = make(example.id, "bob").await;
    let carol = make(example.id, "carol").await;
    let zed = make(other.id, "zed").await;
    let store = DavStore::new(pool.clone());
    for u in [alice, bob, carol, zed] {
        store.ensure_defaults(u).await.unwrap();
    }
    World {
        store,
        alice,
        bob,
        carol,
        zed,
    }
}

fn event(uid: &str, summary: &str, start: &str, end: &str, extra: &str) -> String {
    format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//t//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20250101T000000Z\r\nSUMMARY:{summary}\r\nDTSTART:{start}\r\nDTEND:{end}\r\n{extra}END:VEVENT\r\nEND:VCALENDAR\r\n")
}

fn card(uid: &str, name: &str, email: &str) -> String {
    format!("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{uid}\r\nFN:{name}\r\nN:;{name};;;\r\nEMAIL:{email}\r\nEND:VCARD\r\n")
}

fn at(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

async fn cal(w: &World, user: Uuid) -> Uuid {
    w.store
        .collections(user, Kind::Calendar)
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.access.is_owner())
        .unwrap()
        .id
}

async fn book(w: &World, user: Uuid) -> Uuid {
    w.store
        .collections(user, Kind::AddressBook)
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.access.is_owner())
        .unwrap()
        .id
}

#[sqlx::test(migrations = "../../migrations")]
async fn every_user_gets_a_calendar_and_an_address_book_once(pool: PgPool) {
    let w = world(&pool).await;
    w.store.ensure_defaults(w.alice).await.unwrap();
    let cals = w.store.collections(w.alice, Kind::Calendar).await.unwrap();
    let books = w
        .store
        .collections(w.alice, Kind::AddressBook)
        .await
        .unwrap();
    assert_eq!((cals.len(), books.len()), (1, 1));
    assert_eq!(
        (
            cals[0].slug.as_str(),
            cals[0].display_name.as_str(),
            cals[0].owner_email.as_str(),
            cals[0].access
        ),
        ("default", "Calendar", "alice@example.com", Access::Owner)
    );
    assert_eq!(books[0].slug, "contacts");
}

#[sqlx::test(migrations = "../../migrations")]
async fn objects_are_stored_replaced_and_deleted_with_etags(pool: PgPool) {
    let w = world(&pool).await;
    let c = cal(&w, w.alice).await;
    let v1 = event("u1", "Standup", "20250310T090000Z", "20250310T093000Z", "");
    let first = w
        .store
        .put(w.alice, c, "a.ics", &v1, &Precondition::Absent)
        .await
        .unwrap();
    assert!(first.created);
    let got = w.store.get(w.alice, c, "a.ics").await.unwrap();
    assert_eq!(
        (
            got.data.as_str(),
            got.meta.etag.as_str(),
            got.meta.summary.as_str()
        ),
        (v1.as_str(), first.etag.as_str(), "Standup"),
        "the text is returned exactly as sent"
    );

    // Same text again changes nothing; creating over an existing name is refused.
    let revision = w.store.collection(w.alice, c).await.unwrap().revision;
    assert!(
        !w.store
            .put(w.alice, c, "a.ics", &v1, &Precondition::None)
            .await
            .unwrap()
            .created
    );
    assert_eq!(
        w.store.collection(w.alice, c).await.unwrap().revision,
        revision,
        "an identical write is not a change"
    );
    assert!(matches!(
        w.store
            .put(w.alice, c, "a.ics", &v1, &Precondition::Absent)
            .await,
        Err(DavError::PreconditionFailed)
    ));

    // Update with the right etag works, with a stale one it does not.
    let v2 = event(
        "u1",
        "Standup (moved)",
        "20250310T100000Z",
        "20250310T103000Z",
        "",
    );
    let second = w
        .store
        .put(
            w.alice,
            c,
            "a.ics",
            &v2,
            &Precondition::Etag(first.etag.clone()),
        )
        .await
        .unwrap();
    assert_ne!(second.etag, first.etag);
    assert!(
        matches!(
            w.store
                .put(
                    w.alice,
                    c,
                    "a.ics",
                    &v1,
                    &Precondition::Etag(first.etag.clone())
                )
                .await,
            Err(DavError::PreconditionFailed)
        ),
        "stale etag"
    );
    assert!(
        matches!(
            w.store
                .put(
                    w.alice,
                    c,
                    "missing.ics",
                    &v1,
                    &Precondition::Etag("x".into())
                )
                .await,
            Err(DavError::PreconditionFailed)
        ),
        "If-Match on a missing object"
    );

    // A UID belongs to one object, and an object keeps its UID.
    let clash = event("u1", "Copy", "20250311T090000Z", "20250311T093000Z", "");
    assert!(matches!(
        w.store
            .put(w.alice, c, "b.ics", &clash, &Precondition::None)
            .await,
        Err(DavError::Conflict(_))
    ));
    let renamed = event(
        "other",
        "Changed uid",
        "20250311T090000Z",
        "20250311T093000Z",
        "",
    );
    assert!(matches!(
        w.store
            .put(w.alice, c, "a.ics", &renamed, &Precondition::None)
            .await,
        Err(DavError::Conflict(_))
    ));

    // Bad data and bad names never reach the database.
    assert!(matches!(
        w.store
            .put(w.alice, c, "c.ics", "garbage", &Precondition::None)
            .await,
        Err(DavError::Invalid(_))
    ));
    for name in ["../x.ics", "a b.ics", ".x"] {
        assert!(
            matches!(
                w.store
                    .put(w.alice, c, name, &v1, &Precondition::None)
                    .await,
                Err(DavError::Invalid(_))
            ),
            "{name}"
        );
    }
    assert!(
        matches!(
            w.store
                .put(
                    w.alice,
                    c,
                    "c.vcf",
                    &card("c1", "X", "x@y.de"),
                    &Precondition::None
                )
                .await,
            Err(DavError::Invalid(_))
        ),
        "a contact does not go into a calendar"
    );

    // Delete honours If-Match.
    assert!(matches!(
        w.store.delete(w.alice, c, "a.ics", Some("stale")).await,
        Err(DavError::PreconditionFailed)
    ));
    w.store
        .delete(w.alice, c, "a.ics", Some(&second.etag))
        .await
        .unwrap();
    assert!(matches!(
        w.store.get(w.alice, c, "a.ics").await,
        Err(DavError::NotFound)
    ));
    assert!(matches!(
        w.store.delete(w.alice, c, "a.ics", None).await,
        Err(DavError::NotFound)
    ));
}

#[sqlx::test(migrations = "../../migrations")]
async fn sync_reports_changes_and_deletions_since_a_token(pool: PgPool) {
    let w = world(&pool).await;
    let c = cal(&w, w.alice).await;
    let put = |n: &'static str, uid: &'static str, s: &'static str| {
        let store = w.store.clone();
        let alice = w.alice;
        async move {
            store
                .put(
                    alice,
                    c,
                    n,
                    &event(uid, s, "20250310T090000Z", "20250310T100000Z", ""),
                    &Precondition::None,
                )
                .await
                .unwrap()
        }
    };
    put("a.ics", "ua", "A").await;
    put("b.ics", "ub", "B").await;
    let first = w.store.changes_since(w.alice, c, 0).await.unwrap();
    assert_eq!(
        (first.changed.len(), first.deleted.len()),
        (2, 0),
        "an initial sync lists everything"
    );
    let token = first.revision;

    put("c.ics", "uc", "C").await;
    w.store.delete(w.alice, c, "a.ics", None).await.unwrap();
    put("b.ics", "ub", "B changed").await;
    let next = w.store.changes_since(w.alice, c, token).await.unwrap();
    let mut names: Vec<&str> = next.changed.iter().map(|m| m.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        (names, next.deleted.clone()),
        (vec!["b.ics", "c.ics"], vec!["a.ics".to_owned()])
    );
    assert!(next.revision > token);
    assert!(
        w.store
            .changes_since(w.alice, c, next.revision)
            .await
            .unwrap()
            .changed
            .is_empty(),
        "nothing new since the latest token"
    );
    // Re-creating a deleted name is not reported as deleted any more.
    put("a.ics", "ua", "A again").await;
    assert!(w
        .store
        .changes_since(w.alice, c, token)
        .await
        .unwrap()
        .deleted
        .is_empty());
    assert!(
        matches!(
            w.store.changes_since(w.alice, c, 9999).await,
            Err(DavError::Invalid(_))
        ),
        "a token from the future"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn other_people_cannot_see_or_touch_a_collection(pool: PgPool) {
    let w = world(&pool).await;
    let c = cal(&w, w.alice).await;
    let v = event("u1", "Private", "20250310T090000Z", "20250310T100000Z", "");
    w.store
        .put(w.alice, c, "a.ics", &v, &Precondition::None)
        .await
        .unwrap();
    for outsider in [w.bob, w.zed] {
        assert!(
            matches!(
                w.store.collection(outsider, c).await,
                Err(DavError::NotFound)
            ),
            "its existence is not revealed"
        );
        assert!(matches!(
            w.store.get(outsider, c, "a.ics").await,
            Err(DavError::NotFound)
        ));
        assert!(matches!(
            w.store
                .put(outsider, c, "x.ics", &v, &Precondition::None)
                .await,
            Err(DavError::NotFound)
        ));
        assert!(matches!(
            w.store.delete(outsider, c, "a.ics", None).await,
            Err(DavError::NotFound)
        ));
        assert!(matches!(
            w.store
                .find(outsider, "alice@example.com", Kind::Calendar, "default")
                .await,
            Err(DavError::NotFound)
        ));
        assert!(matches!(
            w.store.changes_since(outsider, c, 0).await,
            Err(DavError::NotFound)
        ));
        assert!(w
            .store
            .collections(outsider, Kind::Calendar)
            .await
            .unwrap()
            .iter()
            .all(|x| x.id != c));
    }
    // The URL's owner part must match, or a lookup would reveal another user's collection.
    assert!(matches!(
        w.store
            .find(w.alice, "bob@example.com", Kind::Calendar, "default")
            .await,
        Err(DavError::NotFound)
    ));
    assert!(
        matches!(
            w.store
                .find(w.alice, "alice@example.com", Kind::AddressBook, "default")
                .await,
            Err(DavError::NotFound)
        ),
        "wrong kind"
    );
    assert!(
        w.store
            .find(w.alice, "ALICE@example.com", Kind::Calendar, "default")
            .await
            .is_ok(),
        "case does not matter"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn sharing_gives_read_or_write_and_can_be_withdrawn(pool: PgPool) {
    let w = world(&pool).await;
    let c = cal(&w, w.alice).await;
    let v = event(
        "u1",
        "Team lunch",
        "20250310T120000Z",
        "20250310T130000Z",
        "",
    );
    w.store
        .put(w.alice, c, "a.ics", &v, &Precondition::None)
        .await
        .unwrap();

    w.store
        .share(
            w.alice,
            c,
            &GranteeRef::UserEmail("Bob@Example.com".into()),
            false,
        )
        .await
        .unwrap();
    let seen = w.store.collections(w.bob, Kind::Calendar).await.unwrap();
    let shared = seen
        .iter()
        .find(|x| x.id == c)
        .expect("bob sees the shared calendar");
    assert_eq!(
        (shared.access, shared.owner_email.as_str()),
        (Access::Read, "alice@example.com")
    );
    assert_eq!(
        w.store.get(w.bob, c, "a.ics").await.unwrap().data,
        v,
        "read-only can read"
    );
    let w2 = event("u2", "Bob's", "20250311T120000Z", "20250311T130000Z", "");
    assert!(
        matches!(
            w.store
                .put(w.bob, c, "b.ics", &w2, &Precondition::None)
                .await,
            Err(DavError::Forbidden)
        ),
        "read-only cannot write"
    );
    assert!(matches!(
        w.store.delete(w.bob, c, "a.ics", None).await,
        Err(DavError::Forbidden)
    ));
    assert!(
        matches!(w.store.shares(w.bob, c).await, Err(DavError::Forbidden)),
        "only the owner sees who has access"
    );
    assert!(
        matches!(
            w.store
                .share(
                    w.bob,
                    c,
                    &GranteeRef::UserEmail("carol@example.com".into()),
                    true
                )
                .await,
            Err(DavError::Forbidden)
        ),
        "a reader cannot share on"
    );
    assert!(matches!(
        w.store
            .update_collection(
                w.bob,
                c,
                &CollectionUpdate {
                    display_name: Some("Mine".into()),
                    ..Default::default()
                }
            )
            .await,
        Err(DavError::Forbidden)
    ));
    assert!(matches!(
        w.store.delete_collection(w.bob, c).await,
        Err(DavError::Forbidden)
    ));
    assert!(
        matches!(
            w.store.get(w.carol, c, "a.ics").await,
            Err(DavError::NotFound)
        ),
        "carol was not given access"
    );

    // Upgrade to read/write.
    w.store
        .share(
            w.alice,
            c,
            &GranteeRef::UserEmail("bob@example.com".into()),
            true,
        )
        .await
        .unwrap();
    assert_eq!(
        w.store.shares(w.alice, c).await.unwrap().len(),
        1,
        "changing the right does not add a second share"
    );
    w.store
        .put(w.bob, c, "b.ics", &w2, &Precondition::None)
        .await
        .unwrap();
    assert_eq!(
        w.store.collection(w.alice, c).await.unwrap().revision,
        w.store.collection(w.bob, c).await.unwrap().revision
    );
    assert!(
        w.store.get(w.alice, c, "b.ics").await.is_ok(),
        "the owner sees what bob added"
    );

    // Withdraw.
    let share = w.store.shares(w.alice, c).await.unwrap().remove(0);
    w.store.unshare(w.alice, c, share.id).await.unwrap();
    assert!(matches!(
        w.store.get(w.bob, c, "a.ics").await,
        Err(DavError::NotFound)
    ));
    assert!(matches!(
        w.store.unshare(w.alice, c, share.id).await,
        Err(DavError::NotFound)
    ));
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_group_share_reaches_its_members_and_the_best_right_wins(pool: PgPool) {
    let w = world(&pool).await;
    let c = cal(&w, w.alice).await;
    w.store
        .put(
            w.alice,
            c,
            "a.ics",
            &event("u1", "Team", "20250310T120000Z", "20250310T130000Z", ""),
            &Precondition::None,
        )
        .await
        .unwrap();
    let domain: Uuid = sqlx::query_scalar("SELECT domain_id FROM users WHERE id = $1")
        .bind(w.alice)
        .fetch_one(&pool)
        .await
        .unwrap();
    let group: Uuid =
        sqlx::query_scalar("INSERT INTO groups (domain_id, name) VALUES ($1, 'Team') RETURNING id")
            .bind(domain)
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query("INSERT INTO group_members (group_id, user_id) VALUES ($1, $2), ($1, $3)")
        .bind(group)
        .bind(w.bob)
        .bind(w.carol)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        w.store.get(w.bob, c, "a.ics").await.is_err(),
        "not before the share"
    );

    w.store
        .share(w.alice, c, &GranteeRef::Group(group), false)
        .await
        .unwrap();
    for member in [w.bob, w.carol] {
        assert_eq!(
            w.store.collection(member, c).await.unwrap().access,
            Access::Read
        );
    }
    // Bob also has a personal read/write share: the higher right applies to him only.
    w.store
        .share(
            w.alice,
            c,
            &GranteeRef::UserEmail("bob@example.com".into()),
            true,
        )
        .await
        .unwrap();
    assert_eq!(
        w.store.collection(w.bob, c).await.unwrap().access,
        Access::ReadWrite
    );
    assert_eq!(
        w.store.collection(w.carol, c).await.unwrap().access,
        Access::Read
    );
    // Leaving the group ends the access that came from it.
    sqlx::query("DELETE FROM group_members WHERE user_id = $1")
        .bind(w.carol)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        w.store.collection(w.carol, c).await,
        Err(DavError::NotFound)
    ));
    let listed = w.store.shares(w.alice, c).await.unwrap();
    assert_eq!(listed.len(), 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn sharing_stays_inside_the_domain(pool: PgPool) {
    let w = world(&pool).await;
    let c = cal(&w, w.alice).await;
    for (what, with) in [
        (
            "another domain",
            GranteeRef::UserEmail("zed@other.org".into()),
        ),
        (
            "unknown",
            GranteeRef::UserEmail("nobody@example.com".into()),
        ),
        (
            "yourself",
            GranteeRef::UserEmail("alice@example.com".into()),
        ),
        ("no such group", GranteeRef::Group(Uuid::new_v4())),
    ] {
        assert!(
            matches!(
                w.store.share(w.alice, c, &with, false).await,
                Err(DavError::Invalid(_))
            ),
            "{what}"
        );
    }
    let other_domain: Uuid = sqlx::query_scalar("SELECT domain_id FROM users WHERE id = $1")
        .bind(w.zed)
        .fetch_one(&pool)
        .await
        .unwrap();
    let foreign_group: Uuid =
        sqlx::query_scalar("INSERT INTO groups (domain_id, name) VALUES ($1, 'Them') RETURNING id")
            .bind(other_domain)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(matches!(
        w.store
            .share(w.alice, c, &GranteeRef::Group(foreign_group), false)
            .await,
        Err(DavError::Invalid(_))
    ));
    assert!(w.store.shares(w.alice, c).await.unwrap().is_empty());
    // A disabled account cannot be shared with.
    sqlx::query("UPDATE users SET status = 'disabled' WHERE id = $1")
        .bind(w.carol)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        w.store
            .share(
                w.alice,
                c,
                &GranteeRef::UserEmail("carol@example.com".into()),
                false
            )
            .await,
        Err(DavError::Invalid(_))
    ));
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_calendar_view_expands_repeats_across_own_and_shared_calendars(pool: PgPool) {
    let w = world(&pool).await;
    let mine = cal(&w, w.bob).await;
    let theirs = cal(&w, w.alice).await;
    w.store
        .put(
            w.bob,
            mine,
            "w.ics",
            &event(
                "w1",
                "Weekly",
                "20250106T090000Z",
                "20250106T100000Z",
                "RRULE:FREQ=WEEKLY;COUNT=10\r\n",
            ),
            &Precondition::None,
        )
        .await
        .unwrap();
    w.store
        .put(
            w.alice,
            theirs,
            "o.ics",
            &event("o1", "Offsite", "20250115T090000Z", "20250115T170000Z", ""),
            &Precondition::None,
        )
        .await
        .unwrap();
    let (from, to) = (at("2025-01-01T00:00:00Z"), at("2025-02-01T00:00:00Z"));
    let own = w.store.calendar_events(w.bob, from, to).await.unwrap();
    assert_eq!(
        own.iter()
            .filter(|e| e.occurrence.summary == "Weekly")
            .count(),
        4,
        "four Mondays in January"
    );
    assert!(
        own.iter().all(|e| e.occurrence.summary != "Offsite"),
        "alice's calendar is not shared yet"
    );
    w.store
        .share(
            w.alice,
            theirs,
            &GranteeRef::UserEmail("bob@example.com".into()),
            false,
        )
        .await
        .unwrap();
    let both = w.store.calendar_events(w.bob, from, to).await.unwrap();
    let offsite = both
        .iter()
        .find(|e| e.occurrence.summary == "Offsite")
        .expect("the shared event");
    assert!(
        !offsite.writable
            && both
                .iter()
                .filter(|e| e.occurrence.summary == "Weekly")
                .all(|e| e.writable)
    );
    assert!(
        both.windows(2)
            .all(|p| p[0].occurrence.start <= p[1].occurrence.start),
        "sorted by time"
    );
    assert!(
        w.store
            .calendar_events(w.bob, from, from + Duration::days(500))
            .await
            .is_err(),
        "windows are bounded"
    );
    // A window that misses an event does not return it.
    assert!(w
        .store
        .calendar_events(
            w.bob,
            at("2025-06-01T00:00:00Z"),
            at("2025-06-30T00:00:00Z")
        )
        .await
        .unwrap()
        .is_empty());
}

#[sqlx::test(migrations = "../../migrations")]
async fn free_busy_shows_times_not_titles_and_only_inside_the_domain(pool: PgPool) {
    let w = world(&pool).await;
    let c = cal(&w, w.alice).await;
    let put =
        |n: &'static str, uid: &'static str, s: &'static str, e: &'static str, x: &'static str| {
            let store = w.store.clone();
            let alice = w.alice;
            async move {
                store
                    .put(
                        alice,
                        c,
                        n,
                        &event(uid, "SECRET TITLE", s, e, x),
                        &Precondition::None,
                    )
                    .await
                    .unwrap()
            }
        };
    put("a.ics", "a", "20250310T090000Z", "20250310T100000Z", "").await;
    put("b.ics", "b", "20250310T093000Z", "20250310T110000Z", "").await;
    put(
        "free.ics",
        "f",
        "20250310T140000Z",
        "20250310T150000Z",
        "TRANSP:TRANSPARENT\r\n",
    )
    .await;
    put(
        "cancelled.ics",
        "c",
        "20250310T160000Z",
        "20250310T170000Z",
        "STATUS:CANCELLED\r\n",
    )
    .await;
    put(
        "daily.ics",
        "d",
        "20250311T120000Z",
        "20250311T123000Z",
        "RRULE:FREQ=DAILY;COUNT=3\r\n",
    )
    .await;
    let (from, to) = (at("2025-03-10T00:00:00Z"), at("2025-03-14T00:00:00Z"));
    let busy = w
        .store
        .free_busy(w.bob, "alice@example.com", from, to)
        .await
        .unwrap();
    assert_eq!(
        busy,
        vec![
            Busy {
                start: at("2025-03-10T09:00:00Z"),
                end: at("2025-03-10T11:00:00Z")
            },
            Busy {
                start: at("2025-03-11T12:00:00Z"),
                end: at("2025-03-11T12:30:00Z")
            },
            Busy {
                start: at("2025-03-12T12:00:00Z"),
                end: at("2025-03-12T12:30:00Z")
            },
            Busy {
                start: at("2025-03-13T12:00:00Z"),
                end: at("2025-03-13T12:30:00Z")
            },
        ],
        "overlaps are merged; free and cancelled events and titles are absent"
    );
    // The window clips events that cross its edges.
    let clipped = w
        .store
        .free_busy(
            w.bob,
            "alice@example.com",
            at("2025-03-10T10:00:00Z"),
            at("2025-03-10T10:30:00Z"),
        )
        .await
        .unwrap();
    assert_eq!(
        clipped,
        vec![Busy {
            start: at("2025-03-10T10:00:00Z"),
            end: at("2025-03-10T10:30:00Z")
        }]
    );
    // Other domains, unknown people and inactive accounts look the same: not found.
    assert!(matches!(
        w.store
            .free_busy(w.zed, "alice@example.com", from, to)
            .await,
        Err(DavError::NotFound)
    ));
    assert!(matches!(
        w.store.free_busy(w.bob, "zed@other.org", from, to).await,
        Err(DavError::NotFound)
    ));
    assert!(matches!(
        w.store
            .free_busy(w.bob, "nobody@example.com", from, to)
            .await,
        Err(DavError::NotFound)
    ));
    sqlx::query("UPDATE users SET status = 'disabled' WHERE id = $1")
        .bind(w.alice)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        w.store
            .free_busy(w.bob, "alice@example.com", from, to)
            .await,
        Err(DavError::NotFound)
    ));
}

#[sqlx::test(migrations = "../../migrations")]
async fn contacts_are_searched_literally(pool: PgPool) {
    let w = world(&pool).await;
    let b = book(&w, w.alice).await;
    for (n, uid, name, email) in [
        ("1.vcf", "c1", "Ada Lovelace", "ada@example.org"),
        ("2.vcf", "c2", "Grace Hopper", "grace@navy.example"),
        ("3.vcf", "c3", "100% Real_Name", "x@y.de"),
    ] {
        w.store
            .put(w.alice, b, n, &card(uid, name, email), &Precondition::None)
            .await
            .unwrap();
    }
    let names = |found: Vec<rustmail_core::dav::store::Object>| {
        found
            .into_iter()
            .map(|o| o.meta.summary)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(
            w.store
                .search_contacts(w.alice, b, "ADA", 50)
                .await
                .unwrap()
        ),
        ["Ada Lovelace"]
    );
    assert_eq!(
        names(
            w.store
                .search_contacts(w.alice, b, "navy.example", 50)
                .await
                .unwrap()
        ),
        ["Grace Hopper"],
        "emails are searched"
    );
    assert_eq!(
        names(w.store.search_contacts(w.alice, b, "", 50).await.unwrap()).len(),
        3,
        "an empty search lists everyone"
    );
    assert_eq!(
        names(w.store.search_contacts(w.alice, b, "%", 50).await.unwrap()),
        ["100% Real_Name"],
        "% is not a wildcard"
    );
    assert_eq!(
        names(w.store.search_contacts(w.alice, b, "_", 50).await.unwrap()),
        ["100% Real_Name"],
        "_ is not a wildcard"
    );
    assert!(
        w.store.search_contacts(w.bob, b, "ada", 50).await.is_err(),
        "another user's address book is not searchable"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn collections_can_be_made_renamed_and_removed_by_their_owner(pool: PgPool) {
    let w = world(&pool).await;
    let work = w
        .store
        .create_collection(
            w.alice,
            Kind::Calendar,
            None,
            "Work Stuff",
            "Meetings",
            Some("#3366cc"),
        )
        .await
        .unwrap();
    assert_eq!(
        (work.slug.as_str(), work.color.as_deref(), work.access),
        ("work-stuff", Some("#3366cc"), Access::Owner)
    );
    let again = w
        .store
        .create_collection(w.alice, Kind::Calendar, None, "Work Stuff", "", None)
        .await
        .unwrap();
    assert_eq!(
        again.slug, "work-stuff-2",
        "a same-named calendar gets its own address"
    );
    assert!(
        matches!(
            w.store
                .create_collection(w.alice, Kind::Calendar, Some("work-stuff"), "X", "", None)
                .await,
            Err(DavError::Conflict(_))
        ),
        "an explicit slug is not changed behind the caller's back"
    );
    for (name, color) in [("", None), ("x", Some("red")), ("x", Some("#12345"))] {
        assert!(
            matches!(
                w.store
                    .create_collection(w.alice, Kind::Calendar, None, name, "", color)
                    .await,
                Err(DavError::Invalid(_))
            ),
            "{name:?} {color:?}"
        );
    }
    w.store
        .update_collection(
            w.alice,
            work.id,
            &CollectionUpdate {
                display_name: Some("Work".into()),
                color: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let c = w.store.collection(w.alice, work.id).await.unwrap();
    assert_eq!(
        (
            c.display_name.as_str(),
            c.color,
            c.description.as_str(),
            c.slug.as_str()
        ),
        ("Work", None, "Meetings", "work-stuff"),
        "the address does not change when renamed"
    );
    w.store
        .put(
            w.alice,
            work.id,
            "a.ics",
            &event("u1", "X", "20250310T090000Z", "20250310T100000Z", ""),
            &Precondition::None,
        )
        .await
        .unwrap();
    w.store.delete_collection(w.alice, work.id).await.unwrap();
    assert!(matches!(
        w.store.collection(w.alice, work.id).await,
        Err(DavError::NotFound)
    ));
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM dav_objects")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 0, "its objects went with it");
}

#[sqlx::test(migrations = "../../migrations")]
async fn app_passwords_log_in_until_revoked_or_the_account_is_off(pool: PgPool) {
    let w = world(&pool).await;
    let apps = AppPasswords::new(pool.clone());
    let token = apps.create(w.alice, "Thunderbird laptop").await.unwrap();
    assert_eq!(token.len(), 32);
    let stored: String = sqlx::query_scalar("SELECT token_hash FROM app_passwords")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        !stored.contains(&token) && stored.len() == 64,
        "only a hash is stored"
    );

    let who = apps
        .verify("Alice@Example.com", &token)
        .await
        .unwrap()
        .expect("valid");
    assert_eq!(
        (who.user_id, who.email.as_str()),
        (w.alice, "alice@example.com")
    );
    assert!(
        apps.verify("bob@example.com", &token)
            .await
            .unwrap()
            .is_none(),
        "the token only works for its owner"
    );
    assert!(apps
        .verify("alice@example.com", "wrong")
        .await
        .unwrap()
        .is_none());
    assert!(apps
        .verify("alice@example.com", "")
        .await
        .unwrap()
        .is_none());
    assert!(apps
        .verify("alice@example.com", &"a".repeat(500))
        .await
        .unwrap()
        .is_none());
    let listed = apps.list(w.alice).await.unwrap();
    assert_eq!(
        (listed.len(), listed[0].name.as_str()),
        (1, "Thunderbird laptop")
    );
    assert!(listed[0].last_used_at.is_some());
    assert!(apps.list(w.bob).await.unwrap().is_empty());

    // Others cannot revoke it.
    assert!(matches!(
        apps.revoke(w.bob, listed[0].id).await,
        Err(DavError::NotFound)
    ));
    assert!(apps
        .verify("alice@example.com", &token)
        .await
        .unwrap()
        .is_some());

    // A disabled account, or a suspended domain, stops working at once.
    sqlx::query("UPDATE users SET status = 'suspended' WHERE id = $1")
        .bind(w.alice)
        .execute(&pool)
        .await
        .unwrap();
    assert!(apps
        .verify("alice@example.com", &token)
        .await
        .unwrap()
        .is_none());
    sqlx::query("UPDATE users SET status = 'active' WHERE id = $1")
        .bind(w.alice)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE domains SET status = 'suspended' WHERE name = 'example.com'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(apps
        .verify("alice@example.com", &token)
        .await
        .unwrap()
        .is_none());
    sqlx::query("UPDATE domains SET status = 'active' WHERE name = 'example.com'")
        .execute(&pool)
        .await
        .unwrap();

    apps.revoke(w.alice, listed[0].id).await.unwrap();
    assert!(
        apps.verify("alice@example.com", &token)
            .await
            .unwrap()
            .is_none(),
        "revoked"
    );
    assert!(apps.list(w.alice).await.unwrap().is_empty());
    let audited: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action LIKE 'app_password.%'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        audited, 2,
        "creating and revoking are audited, the token never is"
    );
    let leaked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE details::text LIKE '%' || $1 || '%'",
    )
    .bind(&token)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(leaked, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn app_passwords_are_limited_and_named_sensibly(pool: PgPool) {
    let w = world(&pool).await;
    let apps = AppPasswords::new(pool.clone());
    assert!(matches!(
        apps.create(w.alice, "  ").await,
        Err(DavError::Invalid(_))
    ));
    assert!(matches!(
        apps.create(w.alice, &"x".repeat(65)).await,
        Err(DavError::Invalid(_))
    ));
    assert!(matches!(
        apps.create(w.alice, "a\nb").await,
        Err(DavError::Invalid(_))
    ));
    for i in 0..MAX_ACTIVE {
        apps.create(w.alice, &format!("app {i}")).await.unwrap();
    }
    assert!(matches!(
        apps.create(w.alice, "one too many").await,
        Err(DavError::Conflict(_))
    ));
    apps.create(w.bob, "bob's").await.unwrap();
    let first = apps.list(w.alice).await.unwrap().pop().unwrap();
    apps.revoke(w.alice, first.id).await.unwrap();
    apps.create(w.alice, "room again").await.unwrap();
}
