//! Access tokens (signed JWTs) and refresh tokens (opaque random strings).
//!
//! Access tokens are short-lived, HS256-signed and carry the user, session and
//! role. Refresh tokens are 256-bit random values; only their SHA-256 hash is
//! ever stored. SHA-256 (rather than a slow password hash) is appropriate here
//! because the input is already high-entropy.

use std::fmt;

use argon2::password_hash::rand_core::{OsRng, RngCore};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::config::AuthConfig;
use super::error::AuthError;
use crate::domain::UserRole;
use crate::error::{Error, Result};

/// Claims carried by an access token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claims {
    /// The authenticated user.
    pub sub: Uuid,
    /// The session this token belongs to; revoking the session kills the token.
    pub sid: Uuid,
    /// The user's role when the token was issued. It can lag behind a role
    /// change by up to the access-token lifetime unless sessions are revoked.
    pub role: UserRole,
    /// Issuer.
    pub iss: String,
    /// Issued-at (seconds since the Unix epoch).
    pub iat: i64,
    /// Expiry (seconds since the Unix epoch).
    pub exp: i64,
}

/// The credentials handed to a client after login or refresh.
#[derive(Clone, PartialEq, Eq)]
pub struct TokenPair {
    /// Signed JWT.
    pub access_token: String,
    /// When the access token expires.
    pub access_expires_at: DateTime<Utc>,
    /// Opaque refresh token. Shown to the client once; only its hash is stored.
    pub refresh_token: String,
    /// When the refresh token (session) expires.
    pub refresh_expires_at: DateTime<Utc>,
    /// The session both tokens belong to.
    pub session_id: Uuid,
}

impl fmt::Debug for TokenPair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenPair")
            .field("access_token", &"<redacted>")
            .field("access_expires_at", &self.access_expires_at)
            .field("refresh_token", &"<redacted>")
            .field("refresh_expires_at", &self.refresh_expires_at)
            .field("session_id", &self.session_id)
            .finish()
    }
}

/// Signs an access token valid from `now` for `config.access_ttl`.
///
/// Returns the token and its expiry.
///
/// # Errors
/// Returns an error if signing fails.
pub fn issue_access_token(
    config: &AuthConfig,
    user_id: Uuid,
    session_id: Uuid,
    role: UserRole,
    now: DateTime<Utc>,
) -> Result<(String, DateTime<Utc>)> {
    let expires_at = now + config.access_ttl;
    let claims = Claims {
        sub: user_id,
        sid: session_id,
        role,
        iss: config.issuer.clone(),
        iat: now.timestamp(),
        exp: expires_at.timestamp(),
    };
    let token = encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(config.secret()),
    )
    .map_err(|e| Error::Config(format!("could not sign access token: {e}")))?;
    Ok((token, expires_at))
}

/// Verifies an access token's signature, issuer and expiry as of `now`.
///
/// Only HS256 is accepted, so `alg: none` and algorithm-swap tokens fail.
/// Expiry is checked against the supplied `now` rather than the system clock,
/// which keeps verification deterministic and testable.
///
/// # Errors
/// Returns [`AuthError::InvalidToken`] for any failure; the reason is
/// deliberately not exposed.
pub fn verify_access_token(
    config: &AuthConfig,
    token: &str,
    now: DateTime<Utc>,
) -> std::result::Result<Claims, AuthError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = false; // checked manually against `now` below
    validation.leeway = 0;
    validation.set_issuer(&[config.issuer.as_str()]);
    validation.set_required_spec_claims(&["exp", "iss", "sub"]);

    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(config.secret()),
        &validation,
    )
    .map_err(|_| AuthError::InvalidToken)?;
    if data.claims.exp <= now.timestamp() {
        return Err(AuthError::InvalidToken);
    }
    Ok(data.claims)
}

/// Reads the claims of a correctly signed access token **without** checking
/// expiry.
///
/// This is only for identifying which session a request belongs to (for CSRF
/// binding, or to log out) when the short-lived token has just lapsed. It is
/// never an authorisation decision: use [`verify_access_token`] for that.
#[must_use]
pub fn peek_claims(config: &AuthConfig, token: &str) -> Option<Claims> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = false;
    validation.leeway = 0;
    validation.set_issuer(&[config.issuer.as_str()]);
    validation.set_required_spec_claims(&["exp", "iss", "sub"]);
    decode::<Claims>(
        token,
        &DecodingKey::from_secret(config.secret()),
        &validation,
    )
    .ok()
    .map(|data| data.claims)
}

/// Generates a refresh token: 256 random bits, URL-safe base64 (43 chars).
#[must_use]
pub fn generate_refresh_token() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Hashes a refresh token for storage and lookup (hex SHA-256).
#[must_use]
pub fn hash_refresh_token(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::*;

    fn config() -> AuthConfig {
        AuthConfig::new("unit-test-signing-secret-32-bytes!!").expect("config")
    }

    fn issue(config: &AuthConfig, now: DateTime<Utc>) -> (String, Uuid, Uuid) {
        let (user, session) = (Uuid::new_v4(), Uuid::new_v4());
        let (token, _) =
            issue_access_token(config, user, session, UserRole::DomainAdmin, now).expect("issue");
        (token, user, session)
    }

    #[test]
    fn access_token_round_trips() {
        let config = config();
        let now = Utc::now();
        let (token, user, session) = issue(&config, now);

        let claims = verify_access_token(&config, &token, now).expect("verify");
        assert_eq!(
            (claims.sub, claims.sid, claims.role),
            (user, session, UserRole::DomainAdmin)
        );
        assert_eq!(claims.exp - claims.iat, config.access_ttl.num_seconds());
    }

    #[test]
    fn token_expires_exactly_at_exp() {
        let config = config();
        let now = Utc::now();
        let (token, ..) = issue(&config, now);

        let just_before = now + config.access_ttl - Duration::seconds(1);
        assert!(verify_access_token(&config, &token, just_before).is_ok());
        let at_expiry = now + config.access_ttl;
        assert!(matches!(
            verify_access_token(&config, &token, at_expiry),
            Err(AuthError::InvalidToken)
        ));
    }

    #[test]
    fn tampered_payload_is_rejected() {
        let config = config();
        let now = Utc::now();
        let (token, ..) = issue(&config, now);
        let mut parts: Vec<&str> = token.split('.').collect();
        let forged_payload = URL_SAFE_NO_PAD.encode(
            serde_json::json!({
                "sub": Uuid::new_v4(), "sid": Uuid::new_v4(), "role": "super_admin",
                "iss": "rustmail", "iat": now.timestamp(), "exp": now.timestamp() + 900,
            })
            .to_string(),
        );
        parts[1] = &forged_payload;
        let forged = parts.join(".");
        assert!(verify_access_token(&config, &forged, now).is_err());
    }

    #[test]
    fn wrong_secret_and_wrong_issuer_are_rejected() {
        let now = Utc::now();
        let (token, ..) = issue(&config(), now);

        let other_secret = AuthConfig::new("a-completely-different-secret-32by").expect("config");
        assert!(verify_access_token(&other_secret, &token, now).is_err());

        let mut other_issuer = config();
        other_issuer.issuer = "someone-else".into();
        assert!(verify_access_token(&other_issuer, &token, now).is_err());
    }

    #[test]
    fn alg_none_and_algorithm_swap_are_rejected() {
        let config = config();
        let now = Utc::now();
        let claims = serde_json::json!({
            "sub": Uuid::new_v4(), "sid": Uuid::new_v4(), "role": "super_admin",
            "iss": "rustmail", "iat": now.timestamp(), "exp": now.timestamp() + 900,
        });

        // `alg: none` with an empty signature.
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
        let none_token = format!("{header}.{payload}.");
        assert!(verify_access_token(&config, &none_token, now).is_err());

        // Correct secret, but signed with HS512 instead of HS256.
        let hs512 = encode(
            &Header::new(Algorithm::HS512),
            &claims,
            &EncodingKey::from_secret(config.secret()),
        )
        .expect("encode");
        assert!(verify_access_token(&config, &hs512, now).is_err());
    }

    #[test]
    fn peek_claims_ignores_expiry_but_not_the_signature() {
        let config = config();
        let long_ago = Utc::now() - Duration::days(30);
        let (token, user, session) = issue(&config, long_ago);

        assert!(
            verify_access_token(&config, &token, Utc::now()).is_err(),
            "expired"
        );
        let claims = peek_claims(&config, &token).expect("signature is valid");
        assert_eq!((claims.sub, claims.sid), (user, session));

        let other = AuthConfig::new("a-completely-different-secret-32by").expect("config");
        assert!(peek_claims(&other, &token).is_none());
        assert!(peek_claims(&config, "garbage").is_none());
    }

    #[test]
    fn garbage_is_rejected() {
        let config = config();
        for bad in ["", "abc", "a.b.c", "....", "Bearer x"] {
            assert!(
                verify_access_token(&config, bad, Utc::now()).is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn refresh_tokens_are_long_unique_and_hash_deterministically() {
        let (a, b) = (generate_refresh_token(), generate_refresh_token());
        assert_eq!(a.len(), 43);
        assert_ne!(a, b);
        assert!(a
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'));

        let hash = hash_refresh_token(&a);
        assert_eq!(hash.len(), 64);
        assert_eq!(hash, hash_refresh_token(&a));
        assert_ne!(hash, hash_refresh_token(&b));
        assert!(!hash.contains(&a), "the hash must not embed the token");
    }

    #[test]
    fn token_pair_debug_redacts_both_tokens() {
        let pair = TokenPair {
            access_token: "access-secret".into(),
            access_expires_at: Utc::now(),
            refresh_token: "refresh-secret".into(),
            refresh_expires_at: Utc::now(),
            session_id: Uuid::nil(),
        };
        let debug = format!("{pair:?}");
        assert!(!debug.contains("access-secret") && !debug.contains("refresh-secret"));
    }
}
