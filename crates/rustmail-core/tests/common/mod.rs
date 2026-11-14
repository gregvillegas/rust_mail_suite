//! Shared helpers for integration tests. `expect`/`unwrap` are fine here:
//! a failing setup step should fail the test loudly.
#![allow(dead_code)]

use rustmail_core::domain::{MailDomain, NewDomain, NewUser, User, UserRole};
use rustmail_core::repository::{
    DomainRepository, PgDomainRepository, PgUserRepository, UserRepository,
};
use sqlx::PgPool;
use uuid::Uuid;

/// A syntactically plausible hash; repository tests do not verify passwords,
/// and real Argon2 hashing would only slow them down.
pub const FAKE_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$ZmFrZXNhbHQ$ZmFrZWhhc2g";

/// Creates a domain with no quota limits.
pub async fn domain(pool: &PgPool, name: &str) -> MailDomain {
    PgDomainRepository::new(pool.clone())
        .create(&NewDomain {
            name: name.into(),
            ..Default::default()
        })
        .await
        .expect("create domain")
}

/// Password used by [`user_with_password`] in auth tests.
pub const PASSWORD: &str = "correct-horse-battery-staple";

/// Creates a user with a real Argon2 hash of [`PASSWORD`], so they can log in.
pub async fn user_with_password(
    pool: &PgPool,
    domain_id: Uuid,
    local_part: &str,
    role: UserRole,
) -> User {
    PgUserRepository::new(pool.clone())
        .create(&NewUser {
            domain_id,
            local_part: local_part.into(),
            password_hash: rustmail_core::password::hash_password(PASSWORD).expect("hash"),
            display_name: String::new(),
            role,
            locale: "en".into(),
        })
        .await
        .expect("create user")
}

/// Creates a user in `domain_id`.
pub async fn user(pool: &PgPool, domain_id: Uuid, local_part: &str, role: UserRole) -> User {
    PgUserRepository::new(pool.clone())
        .create(&NewUser {
            domain_id,
            local_part: local_part.into(),
            password_hash: FAKE_HASH.into(),
            display_name: String::new(),
            role,
            locale: "en".into(),
        })
        .await
        .expect("create user")
}

/// A scratch directory (used as the mail base) removed on drop.
pub struct Scratch(std::path::PathBuf);

impl Scratch {
    pub fn new() -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rustmail-it-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        Self(dir)
    }

    pub fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
