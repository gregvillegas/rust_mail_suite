//! CSRF tokens using the signed double-submit pattern.
//!
//! A token is `<nonce>.<mac>` where `mac = HMAC-SHA256(key, context || nonce)`.
//! The client stores it in a cookie and echoes it in a request header; the
//! server accepts a request only if the two match **and** the MAC verifies for
//! the expected context. Signing defeats cookie-injection attacks (a sibling
//! subdomain planting its own cookie/header pair), and binding the token to a
//! context (the session id, or `"anon"` before login) stops a token from one
//! session being used with another.

use argon2::password_hash::rand_core::{OsRng, RngCore};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use super::config::AuthConfig;
use crate::error::{Error, Result};

type HmacSha256 = Hmac<Sha256>;

/// Context used for tokens issued before a session exists (the login form).
pub const ANONYMOUS_CONTEXT: &str = "anon";

const NONCE_LEN: usize = 24;
const KEY_LABEL: &[u8] = b"rustmail/csrf-key/v1";

/// Issues and verifies CSRF tokens.
#[derive(Clone)]
pub struct CsrfProtector {
    mac: HmacSha256,
}

impl std::fmt::Debug for CsrfProtector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CsrfProtector(<redacted>)")
    }
}

impl CsrfProtector {
    /// Derives a CSRF signing key from the auth secret. The derivation is
    /// domain-separated, so the JWT secret itself is never used as an HMAC key
    /// for CSRF tokens.
    ///
    /// # Errors
    /// Returns [`Error::Config`] if the key cannot be initialised.
    pub fn new(config: &AuthConfig) -> Result<Self> {
        let mut derive = HmacSha256::new_from_slice(config.secret())
            .map_err(|e| Error::Config(format!("invalid CSRF key: {e}")))?;
        derive.update(KEY_LABEL);
        let key = derive.finalize().into_bytes();
        let mac = HmacSha256::new_from_slice(&key)
            .map_err(|e| Error::Config(format!("invalid CSRF key: {e}")))?;
        Ok(Self { mac })
    }

    /// Issues a fresh token bound to `context` (a session id, or
    /// [`ANONYMOUS_CONTEXT`]).
    #[must_use]
    pub fn issue(&self, context: &str) -> String {
        let mut nonce = [0_u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce);
        let nonce = URL_SAFE_NO_PAD.encode(nonce);
        let tag = URL_SAFE_NO_PAD.encode(self.tag(context, &nonce));
        format!("{nonce}.{tag}")
    }

    /// Whether `token` is a genuine token for `context`. Constant-time MAC check.
    #[must_use]
    pub fn verify(&self, context: &str, token: &str) -> bool {
        let Some((nonce, tag)) = token.split_once('.') else {
            return false;
        };
        let Ok(tag) = URL_SAFE_NO_PAD.decode(tag) else {
            return false;
        };
        let mut mac = self.mac.clone();
        mac.update(context.as_bytes());
        mac.update(b"|");
        mac.update(nonce.as_bytes());
        mac.verify_slice(&tag).is_ok()
    }

    fn tag(&self, context: &str, nonce: &str) -> Vec<u8> {
        let mut mac = self.mac.clone();
        mac.update(context.as_bytes());
        mac.update(b"|");
        mac.update(nonce.as_bytes());
        mac.finalize().into_bytes().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn protector(secret: &str) -> CsrfProtector {
        CsrfProtector::new(&AuthConfig::new(secret.as_bytes().to_vec()).expect("config"))
            .expect("protector")
    }

    #[test]
    fn issued_tokens_verify_for_their_context_only() {
        let p = protector("csrf-test-secret-of-at-least-32-bytes");
        let token = p.issue("session-1");
        assert!(p.verify("session-1", &token));
        assert!(!p.verify("session-2", &token), "bound to the session");
        assert!(!p.verify(ANONYMOUS_CONTEXT, &token));
    }

    #[test]
    fn tokens_are_unique() {
        let p = protector("csrf-test-secret-of-at-least-32-bytes");
        assert_ne!(p.issue("anon"), p.issue("anon"));
    }

    #[test]
    fn forged_or_malformed_tokens_fail() {
        let p = protector("csrf-test-secret-of-at-least-32-bytes");
        let token = p.issue("anon");
        let (nonce, _) = token.split_once('.').expect("two parts");

        for bad in [
            "",
            "nodot",
            ".",
            "a.b",
            &format!("{nonce}."),
            &format!("{nonce}.AAAA"),
        ] {
            assert!(!p.verify("anon", bad), "{bad:?} must not verify");
        }
        // A valid MAC for a different nonce does not transfer.
        let other = p.issue("anon");
        let (_, other_tag) = other.split_once('.').expect("two parts");
        assert!(!p.verify("anon", &format!("{nonce}.{other_tag}")));
    }

    #[test]
    fn tokens_from_another_secret_fail() {
        let a = protector("first-secret-that-is-32-bytes-long!!");
        let b = protector("other-secret-that-is-32-bytes-long!!");
        assert!(!b.verify("anon", &a.issue("anon")));
    }
}
