//! Argon2id password hashing.
//!
//! Phase 1 only needs hashing and verification (so seeding never stores a
//! plaintext password). The full authentication service, token handling and
//! lockout policy are built on top of this in Phase 2.

use argon2::password_hash::rand_core::{OsRng, RngCore};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;

use crate::error::{Error, Result};

/// Minimum accepted password length, in characters.
pub const MIN_PASSWORD_LEN: usize = 12;

/// Checks that `password` meets the minimum length.
///
/// Phase 2 extends this into the full password policy.
///
/// # Errors
/// Returns [`Error::Validation`] if the password is too short.
pub fn check_password_length(password: &str) -> Result<()> {
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err(Error::Validation(format!(
            "password must be at least {MIN_PASSWORD_LEN} characters"
        )));
    }
    Ok(())
}

/// Generates a random 32-character hex password (128 bits of entropy) from
/// the operating system's CSPRNG.
#[must_use]
pub fn generate_password() -> String {
    let mut bytes = [0_u8; 16];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Hashes `password` with Argon2id and a fresh random salt.
///
/// Returns the PHC-format string (algorithm, parameters, salt and hash).
///
/// # Errors
/// Returns [`Error::PasswordHash`] if hashing fails.
pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|e| Error::PasswordHash(e.to_string()))
}

/// Verifies `password` against a PHC-format `hash`.
///
/// Returns `Ok(false)` for a wrong password.
///
/// # Errors
/// Returns [`Error::PasswordHash`] if `hash` is malformed.
pub fn verify_password(password: &str, hash: &str) -> Result<bool> {
    let parsed = PasswordHash::new(hash).map_err(|e| Error::PasswordHash(e.to_string()))?;
    match Argon2::default().verify_password(password.as_bytes(), &parsed) {
        Ok(()) => Ok(true),
        Err(argon2::password_hash::Error::Password) => Ok(false),
        Err(e) => Err(Error::PasswordHash(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_then_verify_round_trips() {
        let hash = hash_password("correct horse battery staple").expect("hash");
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify_password("correct horse battery staple", &hash).expect("verify"));
    }

    #[test]
    fn wrong_password_is_rejected() {
        let hash = hash_password("correct horse battery staple").expect("hash");
        assert!(!verify_password("wrong password", &hash).expect("verify"));
    }

    #[test]
    fn same_password_gets_different_salts() {
        let a = hash_password("same-password-123").expect("hash");
        let b = hash_password("same-password-123").expect("hash");
        assert_ne!(a, b);
    }

    #[test]
    fn generated_passwords_are_long_random_and_pass_the_length_check() {
        let a = generate_password();
        let b = generate_password();
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
        assert!(check_password_length(&a).is_ok());
    }

    #[test]
    fn short_passwords_are_rejected() {
        assert!(matches!(
            check_password_length("short"),
            Err(Error::Validation(_))
        ));
        assert!(check_password_length("exactly12chr").is_ok());
    }

    #[test]
    fn malformed_hash_is_an_error() {
        assert!(matches!(
            verify_password("x", "not-a-phc-string"),
            Err(Error::PasswordHash(_))
        ));
    }
}
