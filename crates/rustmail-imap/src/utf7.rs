//! IMAP's "modified UTF-7" encoding for folder names (RFC 3501 §5.1.3).
//!
//! Folder names travel on the wire as 7-bit text: printable ASCII stands for
//! itself, `&` is written `&-`, and any run of other characters is UTF-16 in a
//! base64 variant (`,` instead of `/`, no padding) between `&` and `-`. So the
//! folder "Entwürfe" is `Entw&APw-rfe` on the wire.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,";

fn is_direct(c: char) -> bool {
    ('\u{20}'..='\u{7e}').contains(&c) && c != '&'
}

fn flush(units: &mut Vec<u16>, out: &mut String) {
    if units.is_empty() {
        return;
    }
    let bytes: Vec<u8> = units.iter().flat_map(|u| u.to_be_bytes()).collect();
    out.push('&');
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], chunk.get(1).copied().unwrap_or(0), chunk.get(2).copied().unwrap_or(0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        // 1 byte -> 2 characters, 2 bytes -> 3, 3 bytes -> 4 (no padding).
        for i in 0..=chunk.len() {
            out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 0x3f) as usize]));
        }
    }
    out.push('-');
    units.clear();
}

/// Encodes a folder name for the wire.
#[must_use]
pub fn encode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut pending: Vec<u16> = Vec::new();
    for c in name.chars() {
        if is_direct(c) {
            flush(&mut pending, &mut out);
            out.push(c);
        } else if c == '&' {
            flush(&mut pending, &mut out);
            out.push_str("&-");
        } else {
            let mut buf = [0u16; 2];
            pending.extend_from_slice(c.encode_utf16(&mut buf));
        }
    }
    flush(&mut pending, &mut out);
    out
}

fn sextet(c: u8) -> Option<u32> {
    ALPHABET.iter().position(|&a| a == c).map(|p| p as u32)
}

/// Decodes a folder name from the wire. Returns `None` for malformed input.
#[must_use]
pub fn decode(wire: &str) -> Option<String> {
    let bytes = wire.as_bytes();
    let mut out = String::with_capacity(wire.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b != b'&' {
            if !(0x20..=0x7e).contains(&b) {
                return None;
            }
            out.push(char::from(b));
            i += 1;
            continue;
        }
        let end = i + 1 + bytes[i + 1..].iter().position(|&c| c == b'-')?;
        let body = &bytes[i + 1..end];
        i = end + 1;
        if body.is_empty() {
            out.push('&');
            continue;
        }
        let mut raw: Vec<u8> = Vec::with_capacity(body.len() * 3 / 4);
        let mut acc: u32 = 0;
        let mut bits = 0;
        for &c in body {
            acc = (acc << 6) | sextet(c)?;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                raw.push(((acc >> bits) & 0xff) as u8);
            }
        }
        // Leftover bits must be zero padding, and the data whole UTF-16 units.
        if acc & ((1 << bits) - 1) != 0 || raw.len() % 2 != 0 {
            return None;
        }
        let units: Vec<u16> = raw.chunks(2).map(|p| u16::from_be_bytes([p[0], p[1]])).collect();
        for decoded in char::decode_utf16(units) {
            let c = decoded.ok()?;
            // A shifted run must not spell characters that could have been direct.
            if is_direct(c) || c.is_control() {
                return None;
            }
            out.push(c);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_unchanged_except_the_ampersand() {
        assert_eq!(encode("INBOX"), "INBOX");
        assert_eq!(encode("Work/Projects 2024"), "Work/Projects 2024");
        assert_eq!(encode("Q&A"), "Q&-A");
    }

    #[test]
    fn rfc_3501_examples() {
        // From RFC 3501 section 5.1.3.
        assert_eq!(encode("~peter/mail/台北/日本語"), "~peter/mail/&U,BTFw-/&ZeVnLIqe-");
        assert_eq!(decode("~peter/mail/&U,BTFw-/&ZeVnLIqe-").as_deref(), Some("~peter/mail/台北/日本語"));
    }

    #[test]
    fn umlauts_and_symbols() {
        assert_eq!(encode("Entwürfe"), "Entw&APw-rfe");
        assert_eq!(encode("€"), "&IKw-");
        assert_eq!(decode("Entw&APw-rfe").as_deref(), Some("Entwürfe"));
    }

    #[test]
    fn characters_outside_the_basic_plane_use_surrogates() {
        let name = "Spaß 😀";
        assert_eq!(decode(&encode(name)).as_deref(), Some(name));
    }

    #[test]
    fn round_trips() {
        for name in ["", "a", "&", "&&", "ü", "üü", "üüü", "aüb&c€d", "日本語/テスト", "Trash/ünder & ö"] {
            assert_eq!(decode(&encode(name)).as_deref(), Some(name), "{name:?}");
        }
    }

    #[test]
    fn malformed_input_is_rejected_not_guessed() {
        for bad in ["&", "&APw", "&AP-", "&!!!-", "&APx-x\u{e9}", "caf\u{e9}", "&AAA-", "&QQ-"] {
            assert_eq!(decode(bad), None, "{bad:?}");
        }
    }
}
