//! Normalisation and validation of domain names, local parts and e-mail
//! addresses. Everything is stored lower-case.

use crate::error::{Error, Result};

const MAX_DOMAIN_LEN: usize = 253;
const MAX_LABEL_LEN: usize = 63;
const MAX_LOCAL_PART_LEN: usize = 64;

fn invalid(msg: impl Into<String>) -> Error {
    Error::Validation(msg.into())
}

/// Lower-cases and validates a DNS domain name (at least two labels).
///
/// # Errors
/// Returns [`Error::Validation`] if the name is not a valid host name.
pub fn normalize_domain_name(input: &str) -> Result<String> {
    let name = input.trim().trim_end_matches('.').to_ascii_lowercase();
    if name.is_empty() || name.len() > MAX_DOMAIN_LEN {
        return Err(invalid("domain name must be 1-253 characters"));
    }
    let labels: Vec<&str> = name.split('.').collect();
    if labels.len() < 2 {
        return Err(invalid("domain name must contain at least two labels"));
    }
    for label in labels {
        let valid_chars = label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if label.is_empty()
            || label.len() > MAX_LABEL_LEN
            || !valid_chars
            || label.starts_with('-')
            || label.ends_with('-')
        {
            return Err(invalid(format!("invalid domain label '{label}'")));
        }
    }
    Ok(name)
}

/// Lower-cases and validates the local part of an address (before the `@`).
///
/// Allowed characters are `a-z 0-9 . _ + -`; it may not start or end with a
/// dot or contain consecutive dots.
///
/// # Errors
/// Returns [`Error::Validation`] if the local part is not acceptable.
pub fn normalize_local_part(input: &str) -> Result<String> {
    let local = input.trim().to_ascii_lowercase();
    if local.is_empty() || local.len() > MAX_LOCAL_PART_LEN {
        return Err(invalid("local part must be 1-64 characters"));
    }
    let valid_chars = local.bytes().all(|b| {
        b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'+' | b'-')
    });
    if !valid_chars {
        return Err(invalid(
            "local part may only contain a-z, 0-9, '.', '_', '+', '-'",
        ));
    }
    if local.starts_with('.') || local.ends_with('.') || local.contains("..") {
        return Err(invalid("local part has misplaced dots"));
    }
    Ok(local)
}

/// Splits and normalises an e-mail address into `(local_part, domain)`.
///
/// # Errors
/// Returns [`Error::Validation`] if the address is not `local@domain` with
/// both parts valid.
pub fn split_email(input: &str) -> Result<(String, String)> {
    let trimmed = input.trim();
    let (local, domain) = trimmed
        .split_once('@')
        .ok_or_else(|| invalid("address must contain '@'"))?;
    if domain.contains('@') {
        return Err(invalid("address must contain exactly one '@'"));
    }
    Ok((normalize_local_part(local)?, normalize_domain_name(domain)?))
}

/// Normalises an e-mail address to its canonical lower-case form.
///
/// # Errors
/// Returns [`Error::Validation`] if the address is invalid.
pub fn normalize_email(input: &str) -> Result<String> {
    let (local, domain) = split_email(input)?;
    Ok(format!("{local}@{domain}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_is_lowercased_and_trimmed() {
        assert_eq!(
            normalize_domain_name("  Example.COM. ").expect("ok"),
            "example.com"
        );
        assert_eq!(
            normalize_domain_name("mail.example.co.uk").expect("ok"),
            "mail.example.co.uk"
        );
    }

    #[test]
    fn bad_domains_are_rejected() {
        for bad in [
            "",
            "localhost",
            "-a.com",
            "a-.com",
            "a..com",
            "exa mple.com",
            "a_b.com",
            ".com",
        ] {
            assert!(
                normalize_domain_name(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
        let too_long_label = format!("{}.com", "a".repeat(64));
        assert!(normalize_domain_name(&too_long_label).is_err());
    }

    #[test]
    fn local_part_rules() {
        assert_eq!(
            normalize_local_part("John.Doe+tag").expect("ok"),
            "john.doe+tag"
        );
        for bad in ["", ".a", "a.", "a..b", "a b", "a@b", "ü"] {
            assert!(
                normalize_local_part(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
        assert!(normalize_local_part(&"a".repeat(65)).is_err());
    }

    #[test]
    fn email_is_split_and_normalised() {
        assert_eq!(
            split_email(" Admin@Example.COM ").expect("ok"),
            ("admin".to_owned(), "example.com".to_owned())
        );
        assert_eq!(normalize_email("A@B.io").expect("ok"), "a@b.io");
        for bad in ["admin", "a@b@c.com", "@example.com", "a@"] {
            assert!(split_email(bad).is_err(), "{bad:?} should be rejected");
        }
    }
}
