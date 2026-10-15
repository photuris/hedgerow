//! Pure text helpers: channel secrets, message limits, sender parsing, hex.

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::model::ThreadId;

/// Well-known secret of the `Public` channel (slot 0).
pub const PUBLIC_CHANNEL_SECRET: [u8; 16] = [
    0x8b, 0x33, 0x87, 0xe9, 0xc5, 0xcd, 0xea, 0x6a, 0xc9, 0xe5, 0xed, 0xba, 0xa1, 0x15, 0xcd, 0x72,
];

/// Most bytes of text a direct message can carry.
pub const MAX_DM_BYTES: usize = 150;

/// Scheme prefix of a contact URI.
const CONTACT_SCHEME: &str = "meshcore://";

/// Longest sender name accepted when splitting channel text.
const MAX_SENDER_BYTES: usize = 32;

/// Derives a hashtag channel's secret: `sha256(name)[..16]`.
///
/// # Errors
///
/// Returns [`Error::Invalid`] unless the name starts with `#` and is 2 to
/// 31 bytes long.
pub fn hashtag_secret(name: &str) -> Result<[u8; 16]> {
    if !name.starts_with('#') || !(2..=31).contains(&name.len()) {
        return Err(Error::Invalid(format!(
            "hashtag channel name must start with '#' and be 2 to 31 bytes: {name:?}"
        )));
    }

    let digest = Sha256::digest(name.as_bytes());
    let mut secret = [0u8; 16];
    secret.copy_from_slice(&digest[..16]);

    Ok(secret)
}

/// Returns how many bytes of text the thread can carry.
///
/// A channel message is prefixed by the radio with `"<name>: "`, which
/// eats into the same budget as a direct message.
pub fn max_text_bytes(thread: &ThreadId, self_name: &str) -> usize {
    match thread {
        ThreadId::Contact(_) => MAX_DM_BYTES,
        ThreadId::Channel(_) => MAX_DM_BYTES.saturating_sub(self_name.len() + 2),
    }
}

/// Splits incoming channel text `"<sender>: <text>"` at the first `": "`.
///
/// The sender is returned only when it is 1 to 32 bytes; otherwise the
/// whole text comes back with no sender.
pub fn split_channel_text(text: &str) -> (Option<String>, String) {
    let Some((sender, body)) = text.split_once(": ") else {
        return (None, text.to_string());
    };

    if !(1..=MAX_SENDER_BYTES).contains(&sender.len()) {
        return (None, text.to_string());
    }

    (Some(sender.to_string()), body.to_string())
}

/// Encodes bytes as lowercase hex.
pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Decodes hex of either case.
///
/// # Errors
///
/// Returns [`Error::Invalid`] for odd length or a non-hex character.
pub fn from_hex(s: &str) -> Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return Err(Error::Invalid(format!(
            "odd-length hex ({} chars)",
            s.len()
        )));
    }

    s.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|[hi, lo]| Ok(hex_digit(*hi)? << 4 | hex_digit(*lo)?))
        .collect()
}

/// Decodes one ASCII hex digit.
fn hex_digit(b: u8) -> Result<u8> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(Error::Invalid("non-hex character".to_string())),
    }
}

/// Parses a channel secret: exactly 32 hex characters after trimming.
///
/// # Errors
///
/// Returns [`Error::Invalid`] for any other length or non-hex input.
pub fn parse_secret(s: &str) -> Result<[u8; 16]> {
    let s = s.trim();

    if s.len() != 32 {
        return Err(Error::Invalid(format!(
            "channel secret must be 32 hex characters, got {}",
            s.len()
        )));
    }

    let mut secret = [0u8; 16];
    secret.copy_from_slice(&from_hex(s)?);

    Ok(secret)
}

/// Parses `meshcore://<hex>` (scheme case-insensitive) or bare hex.
///
/// # Errors
///
/// Returns [`Error::Invalid`] when the hex part is malformed.
pub fn parse_contact_uri(s: &str) -> Result<Vec<u8>> {
    let s = s.trim();
    let has_scheme = s
        .get(..CONTACT_SCHEME.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(CONTACT_SCHEME));
    let hex = if has_scheme {
        &s[CONTACT_SCHEME.len()..]
    } else {
        s
    };

    from_hex(hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    mod from_hex {
        use super::*;

        #[test]
        fn should_reject_odd_length() {
            assert!(matches!(from_hex("abc"), Err(Error::Invalid(_))));
        }

        #[test]
        fn should_reject_non_hex_characters() {
            assert!(matches!(from_hex("zz"), Err(Error::Invalid(_))));
            assert!(matches!(from_hex("0g"), Err(Error::Invalid(_))));
        }

        #[test]
        fn should_decode_both_cases() {
            assert_eq!(from_hex("0aFf").unwrap(), vec![0x0a, 0xff]);
        }
    }
}
